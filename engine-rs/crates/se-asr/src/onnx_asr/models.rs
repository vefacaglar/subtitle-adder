//! onnx-asr ASR models: `NemoConformerTdt` (Parakeet TDT v3), `NemoConformerAED` (Canary 1B v2) and
//! `GigaamV3E2eRnnt` (spec transcribe.md §6.6-§6.9), greedy decoding exactly as onnx-asr does it.

use std::collections::HashMap;
use std::path::Path;

use ort::session::Session;
use ort::value::{DynValue, Tensor, TensorElementType};
use se_core::{EngineError, Result};

use super::preproc::{BatchFeatures, GigaamV3Preprocessor, NemoPreprocessor, Preprocessor};
use super::{find_file, read_config, read_vocab};
use crate::ort_runtime::{default_session, ort_err};

/// `TimestampedResult` (the fields the engine reads).
#[derive(Clone, Debug, PartialEq)]
pub struct TimestampedResult {
    pub tokens: Vec<String>,
    /// Seconds relative to the segment start; `None` for Canary.
    pub timestamps: Option<Vec<f64>>,
}

fn input_info(session: &Session, name: &str) -> (Option<TensorElementType>, Vec<i64>) {
    session
        .inputs()
        .iter()
        .find(|i| i.name() == name)
        .map(|i| (i.dtype().tensor_type(), i.dtype().tensor_shape().map(|s| s.to_vec()).unwrap_or_default()))
        .unwrap_or((None, Vec::new()))
}

/// An integer tensor in the element type the model declares (Python lists are converted by onnxruntime).
fn int_tensor(ty: Option<TensorElementType>, shape: Vec<usize>, values: Vec<i64>) -> Result<DynValue> {
    Ok(match ty {
        Some(TensorElementType::Int32) => {
            Tensor::from_array((shape, values.into_iter().map(|v| v as i32).collect::<Vec<i32>>()))
                .map_err(ort_err)?
                .into_dyn()
        }
        _ => Tensor::from_array((shape, values)).map_err(ort_err)?.into_dyn(),
    })
}

fn f32_tensor(shape: Vec<usize>, values: Vec<f32>) -> Result<DynValue> {
    Ok(Tensor::from_array((shape, values)).map_err(ort_err)?.into_dyn())
}

fn extract_f32(v: &DynValue) -> Result<(Vec<i64>, Vec<f32>)> {
    let (shape, data) = v.try_extract_tensor::<f32>().map_err(ort_err)?;
    Ok((shape.to_vec(), data.to_vec()))
}

fn extract_int(v: &DynValue) -> Result<Vec<i64>> {
    if let Ok((_, d)) = v.try_extract_tensor::<i64>() {
        return Ok(d.to_vec());
    }
    let (_, d) = v.try_extract_tensor::<i32>().map_err(ort_err)?;
    Ok(d.iter().map(|&x| x as i64).collect())
}

/// numpy `argmax` (first maximum).
fn argmax(v: &[f32]) -> usize {
    let mut best = 0;
    for (i, &x) in v.iter().enumerate() {
        if x > v[best] || (x.is_nan() && !v[best].is_nan()) {
            best = i;
        }
    }
    best
}

struct Vocab {
    map: HashMap<i64, String>,
    size: usize,
    blank: Option<i64>,
}

impl Vocab {
    fn load(path: &Path) -> Result<Self> {
        let mut map = HashMap::new();
        let mut order: Vec<i64> = Vec::new();
        for (id, tok) in read_vocab(path)? {
            if map.insert(id, tok).is_none() {
                order.push(id);
            }
        }
        // `next(id for id, token in vocab.items() if token == "<blk>")`: dict order = first insertion.
        let blank = order.iter().copied().find(|id| map[id] == "<blk>");
        let size = map.len();
        Ok(Self { map, size, blank })
    }

    fn token(&self, id: i64) -> Result<String> {
        self.map.get(&id).cloned().ok_or_else(|| EngineError::key(&id.to_string()))
    }
}

/// `[B, D, T']` encoder output transposed to per-item frame vectors.
fn transpose_bdt(shape: &[i64], data: &[f32]) -> (usize, usize, usize, Vec<f32>) {
    let (b, d, t) = (shape[0] as usize, shape[1] as usize, shape[2] as usize);
    let mut out = vec![0f32; b * t * d];
    for bi in 0..b {
        for di in 0..d {
            for ti in 0..t {
                out[(bi * t + ti) * d + di] = data[(bi * d + di) * t + ti];
            }
        }
    }
    (b, t, d, out)
}

/// One onnx-asr ASR model.
pub trait AsrModel {
    /// `asr.recognize_batch(waveforms, lens, language=..., target_language=..., need_logprobs=True)`.
    fn recognize_batch(
        &mut self,
        waveforms: &[f32],
        batch: usize,
        n: usize,
        lens: &[i64],
        language: Option<&str>,
        target_language: Option<&str>,
    ) -> Result<Vec<TimestampedResult>>;
}

fn encoder_inputs(features: &BatchFeatures) -> Result<(DynValue, DynValue)> {
    let x = f32_tensor(vec![features.batch, features.n_feat, features.frames], features.data.clone())?;
    let l = Tensor::from_array(([features.batch], features.lens.clone())).map_err(ort_err)?.into_dyn();
    Ok((x, l))
}

fn timestamps(window_step_factor: f64, indices: &[usize]) -> Vec<f64> {
    indices.iter().map(|&i| window_step_factor * i as f64).collect()
}

/// `NemoConformerTdt` (Parakeet TDT).
pub struct ParakeetTdt {
    encoder: Session,
    decoder_joint: Session,
    vocab: Vocab,
    pre: NemoPreprocessor,
    subsampling: usize,
    max_tokens_per_step: usize,
}

impl ParakeetTdt {
    pub fn load(dir: &Path, quantization: Option<&str>) -> Result<Self> {
        let suffix = quantization.map(|q| format!("?{q}")).unwrap_or_default();
        let config = read_config(dir)?;
        let enc = find_file(dir, &format!("encoder-model{suffix}.onnx"))?;
        let dec = find_file(dir, &format!("decoder_joint-model{suffix}.onnx"))?;
        let vocab = find_file(dir, "vocab.txt")?;
        let vocab = Vocab::load(&vocab)?;
        let get = |k: &str, d: i64| config.get(k).and_then(|v| v.as_i64()).unwrap_or(d) as usize;
        Ok(Self {
            subsampling: get("subsampling_factor", 8),
            max_tokens_per_step: get("max_tokens_per_step", 10),
            encoder: default_session(&enc)?,
            decoder_joint: default_session(&dec)?,
            vocab,
            pre: NemoPreprocessor::new(),
        })
    }
}

impl AsrModel for ParakeetTdt {
    fn recognize_batch(
        &mut self,
        waveforms: &[f32],
        batch: usize,
        n: usize,
        lens: &[i64],
        _language: Option<&str>,
        _target_language: Option<&str>,
    ) -> Result<Vec<TimestampedResult>> {
        let features = self.pre.process(waveforms, batch, n, lens);
        let (x, l) = encoder_inputs(&features)?;
        let outputs = self.encoder.run(ort::inputs!["audio_signal" => x, "length" => l]).map_err(ort_err)?;
        let (shape, data) = extract_f32(&outputs["outputs"])?;
        let enc_lens = extract_int(&outputs["encoded_lengths"])?;
        drop(outputs);
        let (b, t_max, d, enc) = transpose_bdt(&shape, &data);
        let blank = self.vocab.blank.unwrap_or(-1);
        let (t_ty, _) = input_info(&self.decoder_joint, "targets");
        let (l_ty, _) = input_info(&self.decoder_joint, "target_length");
        let (_, s1) = input_info(&self.decoder_joint, "input_states_1");
        let (_, s2) = input_info(&self.decoder_joint, "input_states_2");
        let s1_shape = vec![s1[0].max(0) as usize, 1, s1[2].max(0) as usize];
        let s2_shape = vec![s2[0].max(0) as usize, 1, s2[2].max(0) as usize];
        let mut results = Vec::with_capacity(b);
        for bi in 0..b {
            let len = enc_lens[bi].max(0) as usize;
            let mut state = (vec![0f32; s1_shape.iter().product()], vec![0f32; s2_shape.iter().product()]);
            let mut tokens: Vec<i64> = Vec::new();
            let mut ts: Vec<usize> = Vec::new();
            let mut t = 0usize;
            let mut emitted = 0usize;
            while t < len.min(t_max) {
                let frame = enc[(bi * t_max + t) * d..(bi * t_max + t + 1) * d].to_vec();
                let target = *tokens.last().unwrap_or(&blank);
                let inputs = ort::inputs![
                    "encoder_outputs" => f32_tensor(vec![1, d, 1], frame)?,
                    "targets" => int_tensor(t_ty, vec![1, 1], vec![target])?,
                    "target_length" => int_tensor(l_ty, vec![1], vec![1])?,
                    "input_states_1" => f32_tensor(s1_shape.clone(), state.0.clone())?,
                    "input_states_2" => f32_tensor(s2_shape.clone(), state.1.clone())?,
                ];
                let out = self.decoder_joint.run(inputs).map_err(ort_err)?;
                let (_, logits_all) = extract_f32(&out["outputs"])?;
                let (_, ns1) = extract_f32(&out["output_states_1"])?;
                let (_, ns2) = extract_f32(&out["output_states_2"])?;
                let vs = self.vocab.size.min(logits_all.len());
                let token = argmax(&logits_all[..vs]) as i64;
                let step = if logits_all.len() > vs { argmax(&logits_all[vs..]) } else { 0 };
                if token != blank {
                    state = (ns1, ns2);
                    tokens.push(token);
                    ts.push(t);
                    emitted += 1;
                }
                if step > 0 {
                    t += step;
                    emitted = 0;
                } else if token == blank || emitted == self.max_tokens_per_step {
                    t += 1;
                    emitted = 0;
                }
            }
            let toks = tokens.iter().map(|&i| self.vocab.token(i)).collect::<Result<Vec<_>>>()?;
            results.push(TimestampedResult { tokens: toks, timestamps: Some(timestamps(0.01 * self.subsampling as f64, &ts)) });
        }
        Ok(results)
    }
}

/// GigaAM RNN-T decoder state (onnx-asr's list that caches the decoder output after a blank).
enum DecState {
    Fresh { h: Vec<f32>, c: Vec<f32> },
    Cached { dec: (Vec<i64>, Vec<f32>), h: Vec<f32>, c: Vec<f32> },
}

/// `GigaamV3E2eRnnt`.
pub struct GigaamRnnt {
    encoder: Session,
    decoder: Session,
    joint: Session,
    vocab: Vocab,
    pre: GigaamV3Preprocessor,
    subsampling: usize,
    max_tokens_per_step: usize,
}

const PRED_HIDDEN: usize = 320;

impl GigaamRnnt {
    pub fn load(dir: &Path, quantization: Option<&str>) -> Result<Self> {
        let suffix = quantization.map(|q| format!("?{q}")).unwrap_or_default();
        let config = read_config(dir)?;
        let enc = find_file(dir, &format!("v3_e2e_rnnt_encoder{suffix}.onnx"))?;
        let dec = find_file(dir, &format!("v3_e2e_rnnt_decoder{suffix}.onnx"))?;
        let joint = find_file(dir, &format!("v3_e2e_rnnt_joint{suffix}.onnx"))?;
        let vocab = Vocab::load(&find_file(dir, "v3_e2e_rnnt_vocab.txt")?)?;
        let get = |k: &str, d: i64| config.get(k).and_then(|v| v.as_i64()).unwrap_or(d) as usize;
        Ok(Self {
            subsampling: get("subsampling_factor", 4),
            max_tokens_per_step: get("max_tokens_per_step", 3),
            encoder: default_session(&enc)?,
            decoder: default_session(&dec)?,
            joint: default_session(&joint)?,
            vocab,
            pre: GigaamV3Preprocessor::new(),
        })
    }
}

impl AsrModel for GigaamRnnt {
    fn recognize_batch(
        &mut self,
        waveforms: &[f32],
        batch: usize,
        n: usize,
        lens: &[i64],
        _language: Option<&str>,
        _target_language: Option<&str>,
    ) -> Result<Vec<TimestampedResult>> {
        let features = self.pre.process(waveforms, batch, n, lens);
        let (x, l) = encoder_inputs(&features)?;
        let outputs = self.encoder.run(ort::inputs!["audio_signal" => x, "length" => l]).map_err(ort_err)?;
        let (shape, data) = extract_f32(&outputs["encoded"])?;
        let enc_lens = extract_int(&outputs["encoded_len"])?;
        drop(outputs);
        let (b, t_max, d, enc) = transpose_bdt(&shape, &data);
        let blank = self.vocab.blank.unwrap_or(-1);
        let (x_ty, _) = input_info(&self.decoder, "x");
        let mut results = Vec::with_capacity(b);
        for bi in 0..b {
            let len = enc_lens[bi].max(0) as usize;
            let mut state = DecState::Fresh { h: vec![0f32; PRED_HIDDEN], c: vec![0f32; PRED_HIDDEN] };
            let mut tokens: Vec<i64> = Vec::new();
            let mut ts: Vec<usize> = Vec::new();
            let mut t = 0usize;
            let mut emitted = 0usize;
            while t < len.min(t_max) {
                let (dec, h2, c2) = match &state {
                    DecState::Fresh { h, c } => {
                        let target = *tokens.last().unwrap_or(&blank);
                        let out = self
                            .decoder
                            .run(ort::inputs![
                                "x" => int_tensor(x_ty, vec![1, 1], vec![target])?,
                                "h.1" => f32_tensor(vec![1, 1, PRED_HIDDEN], h.clone())?,
                                "c.1" => f32_tensor(vec![1, 1, PRED_HIDDEN], c.clone())?,
                            ])
                            .map_err(ort_err)?;
                        let dec = extract_f32(&out["dec"])?;
                        let (_, h2) = extract_f32(&out["h"])?;
                        let (_, c2) = extract_f32(&out["c"])?;
                        (dec, h2, c2)
                    }
                    DecState::Cached { dec, h, c } => (dec.clone(), h.clone(), c.clone()),
                };
                // The list is mutated in place to [dec, h', c'] (seen again after a blank).
                state = DecState::Cached { dec: dec.clone(), h: h2.clone(), c: c2.clone() };
                // dec: [1, 1, H] (or [1, H, 1]); transpose(0, 2, 1).
                let (dshape, ddata) = dec;
                let dec_t_shape = if dshape.len() == 3 {
                    vec![dshape[0] as usize, dshape[2] as usize, dshape[1] as usize]
                } else {
                    dshape.iter().map(|&v| v as usize).collect()
                };
                let dec_t = if dshape.len() == 3 {
                    let (a, bb, cc) = (dshape[0] as usize, dshape[1] as usize, dshape[2] as usize);
                    let mut o = vec![0f32; ddata.len()];
                    for i in 0..a {
                        for j in 0..bb {
                            for k in 0..cc {
                                o[(i * cc + k) * bb + j] = ddata[(i * bb + j) * cc + k];
                            }
                        }
                    }
                    o
                } else {
                    ddata
                };
                let frame = enc[(bi * t_max + t) * d..(bi * t_max + t + 1) * d].to_vec();
                let out = self
                    .joint
                    .run(ort::inputs![
                        "enc" => f32_tensor(vec![1, d, 1], frame)?,
                        "dec" => f32_tensor(dec_t_shape, dec_t)?,
                    ])
                    .map_err(ort_err)?;
                let (_, logits) = extract_f32(&out["joint"])?;
                let token = argmax(&logits) as i64;
                if token != blank {
                    state = DecState::Fresh { h: h2, c: c2 };
                    tokens.push(token);
                    ts.push(t);
                    emitted += 1;
                }
                if token == blank || emitted == self.max_tokens_per_step {
                    t += 1;
                    emitted = 0;
                }
            }
            let toks = tokens.iter().map(|&i| self.vocab.token(i)).collect::<Result<Vec<_>>>()?;
            results.push(TimestampedResult { tokens: toks, timestamps: Some(timestamps(0.01 * self.subsampling as f64, &ts)) });
        }
        Ok(results)
    }
}

/// `NemoConformerAED` (Canary).
pub struct CanaryAed {
    encoder: Session,
    decoder: Session,
    vocab: Vocab,
    tokens: HashMap<String, i64>,
    pre: NemoPreprocessor,
    max_sequence_length: usize,
}

impl CanaryAed {
    pub fn load(dir: &Path, quantization: Option<&str>) -> Result<Self> {
        let suffix = quantization.map(|q| format!("?{q}")).unwrap_or_default();
        let config = read_config(dir)?;
        let enc = find_file(dir, &format!("encoder-model{suffix}.onnx"))?;
        let dec = find_file(dir, &format!("decoder-model{suffix}.onnx"))?;
        let vocab = Vocab::load(&find_file(dir, "vocab.txt")?)?;
        // {token: id for id, token in vocab.items()}: the last id of a duplicated token wins.
        let mut ids: Vec<(&i64, &String)> = vocab.map.iter().collect();
        ids.sort_by_key(|(id, _)| **id);
        let mut tokens = HashMap::new();
        for (id, tok) in ids {
            tokens.insert(tok.clone(), *id);
        }
        let max_sequence_length =
            config.get("max_sequence_length").and_then(|v| v.as_i64()).unwrap_or(1024) as usize;
        Ok(Self {
            encoder: default_session(&enc)?,
            decoder: default_session(&dec)?,
            vocab,
            tokens,
            pre: NemoPreprocessor::new(),
            max_sequence_length,
        })
    }

    fn id(&self, token: &str) -> Result<i64> {
        self.tokens.get(token).copied().ok_or_else(|| EngineError::key(token))
    }
}

impl AsrModel for CanaryAed {
    fn recognize_batch(
        &mut self,
        waveforms: &[f32],
        batch: usize,
        n: usize,
        lens: &[i64],
        language: Option<&str>,
        target_language: Option<&str>,
    ) -> Result<Vec<TimestampedResult>> {
        let mut prefix = vec![
            self.id(" ")?,
            self.id("<|startofcontext|>")?,
            self.id("<|startoftranscript|>")?,
            self.id("<|emo:undefined|>")?,
            self.id("<|en|>")?,
            self.id("<|en|>")?,
            self.id("<|pnc|>")?,
            self.id("<|noitn|>")?,
            self.id("<|notimestamp|>")?,
            self.id("<|nodiarize|>")?,
        ];
        let language = language.filter(|l| !l.is_empty());
        if let Some(l) = language {
            prefix[4] = self.id(&format!("<|{l}|>"))?;
        }
        if let Some(tl) = target_language.filter(|l| !l.is_empty()).or(language) {
            prefix[5] = self.id(&format!("<|{tl}|>"))?;
        }
        let eos = self.id("<|endoftext|>")?;

        let features = self.pre.process(waveforms, batch, n, lens);
        let (x, l) = encoder_inputs(&features)?;
        let outputs = self.encoder.run(ort::inputs!["audio_signal" => x, "length" => l]).map_err(ort_err)?;
        let (emb_shape, emb) = extract_f32(&outputs["encoder_embeddings"])?;
        let (mask_shape, mask) = {
            let (s, d) = outputs["encoder_mask"].try_extract_tensor::<i64>().map_err(ort_err)?;
            (s.to_vec(), d.to_vec())
        };
        drop(outputs);
        let (_, mems_dims) = input_info(&self.decoder, "decoder_mems");
        let b = emb_shape[0] as usize;
        let prefix_len = prefix.len();
        let mut seqs: Vec<Vec<i64>> = vec![prefix.clone(); b];
        let mut mems_shape = vec![mems_dims[0].max(0) as usize, b, 0, mems_dims[3].max(0) as usize];
        let mut mems: Vec<f32> = Vec::new();
        let us = |s: &[i64]| s.iter().map(|&v| v as usize).collect::<Vec<usize>>();
        while seqs[0].len() < self.max_sequence_length {
            let first = mems_shape[2] == 0;
            let cols = if first { seqs[0].len() } else { 1 };
            let ids: Vec<i64> = seqs.iter().flat_map(|s| s[s.len() - cols..].to_vec()).collect();
            let out = self
                .decoder
                .run(ort::inputs![
                    "input_ids" => Tensor::from_array((vec![b, cols], ids)).map_err(ort_err)?.into_dyn(),
                    "encoder_embeddings" => f32_tensor(us(&emb_shape), emb.clone())?,
                    "encoder_mask" => Tensor::from_array((us(&mask_shape), mask.clone())).map_err(ort_err)?.into_dyn(),
                    "decoder_mems" => f32_tensor(mems_shape.clone(), mems.clone())?,
                ])
                .map_err(ort_err)?;
            let (lshape, logits) = extract_f32(&out["logits"])?;
            let (nshape, nmems) = extract_f32(&out["decoder_hidden_states"])?;
            drop(out);
            let (lb, ll, lv) = (lshape[0] as usize, lshape[1] as usize, lshape[2] as usize);
            let next: Vec<i64> = (0..lb)
                .map(|bi| {
                    let row = &logits[(bi * ll + ll - 1) * lv..(bi * ll + ll) * lv];
                    argmax(row) as i64
                })
                .collect();
            mems_shape = us(&nshape);
            mems = nmems;
            if next.iter().all(|&t| t == eos) {
                break;
            }
            for (s, t) in seqs.iter_mut().zip(next) {
                s.push(t);
            }
        }
        let mut results = Vec::with_capacity(b);
        for s in seqs {
            let mut toks = Vec::new();
            for &id in &s[prefix_len..] {
                let tok = self.vocab.token(id)?;
                if !tok.starts_with("<|") {
                    toks.push(tok);
                }
            }
            results.push(TimestampedResult { tokens: toks, timestamps: None });
        }
        Ok(results)
    }
}

/// `onnx_asr.load_model(name, quantization)` for the three catalog models.
pub fn load_model(name: &str, quantization: Option<&str>) -> Result<Box<dyn AsrModel>> {
    crate::ort_runtime::init()?;
    let dir = super::model_dir(name)?;
    Ok(match name {
        "nemo-parakeet-tdt-0.6b-v3" => Box::new(ParakeetTdt::load(&dir, quantization)?),
        "nemo-canary-1b-v2" => Box::new(CanaryAed::load(&dir, quantization)?),
        "gigaam-v3-e2e-rnnt" => Box::new(GigaamRnnt::load(&dir, quantization)?),
        other => return Err(EngineError::value(format!("Model '{other}' not supported!"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argmax_takes_the_first_maximum() {
        assert_eq!(argmax(&[0.1, 0.5, 0.5, 0.2]), 1);
    }

    #[test]
    fn transposes_encoder_output() {
        let (b, t, d, out) = transpose_bdt(&[1, 2, 3], &[1., 2., 3., 4., 5., 6.]);
        assert_eq!((b, t, d), (1, 3, 2));
        assert_eq!(out, vec![1., 4., 2., 5., 3., 6.]);
    }
}

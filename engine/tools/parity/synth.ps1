param([string]$Out)
# Writes the raw two-voice SAPI speech WAVs (16 kHz mono) for the parity samples; synth.sh then builds the videos.
Add-Type -AssemblyName System.Speech
New-Item -ItemType Directory -Force $Out | Out-Null

function Say([string]$voice, [string]$text, [string]$path, [int]$rate = 0) {
    $s = New-Object System.Speech.Synthesis.SpeechSynthesizer
    $s.SelectVoice($voice)
    $s.Rate = $rate
    $fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)
    $s.SetOutputToWaveFile($path, $fmt)
    $s.Speak($text)
    $s.Dispose()
}

$en = @(
  @('Microsoft Zira Desktop', 'Hello, how are you doing today? I wanted to ask you about the project.'),
  @('Microsoft David Desktop', 'I am doing well, thank you. The project is going fine, we finished the first part yesterday.'),
  @('Microsoft Zira Desktop', 'That is great news. When do you think the second part will be ready?'),
  @('Microsoft David Desktop', 'I think we will need about two more weeks. There are still a few problems to solve.'),
  @('Microsoft Zira Desktop', 'Okay, that sounds reasonable. Please keep me updated.'),
  @('Microsoft David Desktop', 'Of course. I will send you a short report on Friday.')
)
# Turkish has one SAPI voice (Tolga); synth.sh pitch-shifts turns 2, 4, 6 as a second speaker.
$tr = @(
  @(0, 'Merhaba, bugün nasılsın? Sana proje hakkında bir şey sormak istiyordum.'),
  @(1, 'İyiyim, teşekkür ederim. Proje iyi gidiyor, ilk bölümü dün bitirdik.'),
  @(0, 'Bu harika bir haber. İkinci bölüm sence ne zaman hazır olur?'),
  @(1, 'Sanırım iki hafta daha gerekecek. Çözmemiz gereken birkaç sorun var.'),
  @(0, 'Tamam, makul görünüyor. Lütfen beni haberdar et.'),
  @(1, 'Elbette. Cuma günü sana kısa bir rapor göndereceğim.')
)

$i=0; foreach ($p in $en) { $i++; Say $p[0] $p[1] (Join-Path $Out "en-$i.wav") }
$i=0; foreach ($p in $tr) { $i++; Say 'Microsoft Tolga' $p[1] (Join-Path $Out "tr-$i-raw.wav") }

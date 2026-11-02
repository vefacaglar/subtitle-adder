"""External LLM server: where an API key may travel, what the subnet scan sends, and what strict offline mode allows."""
import http.server
import io
import json
import socket
import sys
import threading
import urllib.request

import pytest

from subtitle_engine import cli, i18n, models, translate
from subtitle_engine.models import ModelError

KEY = "sk-test-secret-key-123"
KEY_ERROR_TR = "API anahtarı şifrelenmemiş bağlantıyla (http) gönderilmez; adresi https:// ile girin."
KEY_ERROR_EN = "The API key is not sent over an unencrypted (http) connection; enter the address with https://."

LOCAL_HOSTS = [
    "localhost", "LOCALHOST", "app.localhost", "127.0.0.1", "127.1", "0x7f.1", "2130706433", "[::1]", "::1",
    "10.1.2.3", "172.16.0.5", "172.31.255.254", "192.168.1.42", "100.64.0.1", "100.127.255.254", "169.254.10.20",
    "fd12:3456::1", "fc00::1", "fe80::1", "fe80::1%en0", "::ffff:192.168.1.5",
    "mac-studio.local", "MAC-STUDIO.LOCAL", "mac-studio.local.", "mac-studio", "ollama",
]
REMOTE_HOSTS = [
    "api.openai.com", "opencode.ai", "openrouter.ai", "generativelanguage.googleapis.com", "example.com",
    "8.8.8.8", "134744072", "172.32.0.1", "172.15.255.255", "100.128.0.1", "100.63.255.255", "192.169.0.1",
    "11.0.0.1", "169.255.0.1", "2001:4860:4860::8888", "fec0::1", "::ffff:8.8.8.8",
    "10.0.0.1.nip.io", "localhost.example.com", "mac-studio.local.example.com", "", None,
]


@pytest.mark.parametrize("host", LOCAL_HOSTS)
def test_local_network_hosts(host):
    assert translate.is_local_network_host(host)


@pytest.mark.parametrize("host", REMOTE_HOSTS)
def test_internet_hosts(host):
    assert not translate.is_local_network_host(host)


@pytest.mark.parametrize(
    ("url", "allowed"),
    [
        ("https://api.openai.com/v1/chat/completions", True),
        ("HTTPS://opencode.ai/zen/go/v1/chat/completions", True),
        ("http://192.168.1.42:1234/v1/chat/completions", True),
        ("http://mac-studio.local:8080/v1/chat/completions", True),
        ("http://[fd00::1]:8080/v1/chat/completions", True),
        ("http://ollama:11434/api/chat", True),
        ("http://api.openai.com/v1/chat/completions", False),
        ("http://203.0.113.9:1234/v1/chat/completions", False),
        ("http://[2001:db8::1]/v1", False),
        ("ftp://192.168.1.42/v1", False),
        ("not a url", False),
    ],
)
def test_api_key_transport_rule(url, allowed):
    assert translate.api_key_transport_allowed(url) is allowed


class FakeHttpResponse:
    def __init__(self, payload: dict):
        self._payload = payload

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False

    def read(self) -> bytes:
        return json.dumps(self._payload).encode("utf-8")


CHAT_REPLY = {"choices": [{"message": {"content": "[1] Merhaba!"}}]}
MESSAGES = [{"role": "user", "content": "hi"}]


def recording_urlopen(monkeypatch) -> list:
    requests: list = []

    def fake_urlopen(req, timeout=None):
        requests.append(req)
        return FakeHttpResponse(CHAT_REPLY)

    monkeypatch.setattr(translate.urllib.request, "urlopen", fake_urlopen)
    return requests


def forbid_requests(monkeypatch):
    def fail(*args, **kwargs):
        pytest.fail("no request may be made")

    monkeypatch.setattr(translate.urllib.request, "urlopen", fail)


@pytest.mark.parametrize(
    "url", ["http://api.openai.com/v1", "api.openai.com", "http://203.0.113.9:1234", "http://openrouter.ai/api/v1"]
)
def test_api_key_over_plain_http_to_the_internet_is_refused_before_any_request(monkeypatch, url):
    forbid_requests(monkeypatch)

    with pytest.raises(ModelError) as tr:
        translate.LlamaTranslator("madlad-3b", external_url=url, external_model="m", external_api_key=KEY)
    assert str(tr.value) == KEY_ERROR_TR
    assert KEY not in str(tr.value)

    monkeypatch.setenv(i18n.ENV_VAR, "en")
    with pytest.raises(ModelError) as en:
        translate.LlamaTranslator("madlad-3b", external_url=url, external_model="m", external_api_key=KEY)
    assert str(en.value) == KEY_ERROR_EN


def test_api_key_over_plain_http_is_refused_from_the_environment_through_the_cli(monkeypatch, capsys):
    forbid_requests(monkeypatch)
    monkeypatch.setenv(translate.EXTERNAL_LLM_URL_ENV_VAR, "http://api.openai.com/v1")
    monkeypatch.setenv(translate.EXTERNAL_LLM_API_KEY_ENV_VAR, KEY)
    monkeypatch.setattr(sys, "stdin", io.StringIO("[]"))

    assert cli.main(["translate", "--to", "tr", "--json"]) == 1

    out = capsys.readouterr().out
    assert json.loads(out) == {"event": "error", "message": KEY_ERROR_TR}  # and no stage event before it
    assert KEY not in out


@pytest.mark.parametrize(
    "url",
    [
        "https://api.openai.com/v1",
        "http://192.168.1.42:1234",
        "http://10.0.0.5:1234",
        "http://100.100.1.1:1234",
        "http://127.0.0.1:1234",
        "http://localhost:11434",
        "http://[fd00::1]:1234",
        "http://mac-studio.local:1234",
        "http://ollama:11434",
        "mac-studio:1234",
    ],
)
def test_api_key_is_sent_over_https_or_to_the_local_network(monkeypatch, url):
    monkeypatch.setattr(translate, "_resolve_lan_url", lambda u, **kwargs: u)
    requests = recording_urlopen(monkeypatch)
    translator = translate.LlamaTranslator("madlad-3b", external_url=url, external_model="m", external_api_key=KEY)

    translator._complete(MESSAGES)

    assert requests[0].get_header("Authorization") == f"Bearer {KEY}"


def test_plain_http_to_the_internet_without_a_key_is_unchanged(monkeypatch):
    requests = recording_urlopen(monkeypatch)
    translator = translate.LlamaTranslator("madlad-3b", external_url="http://api.example.com:8080", external_model="m")

    translator._complete(MESSAGES)

    assert requests[0].full_url == "http://api.example.com:8080/v1/chat/completions"
    assert requests[0].get_header("Authorization") is None


def test_the_key_is_checked_against_the_url_after_local_name_resolution(monkeypatch):
    """A `.local` name that resolves to a public address must not receive the key over plain http."""
    forbid_requests(monkeypatch)
    monkeypatch.setattr(translate, "_resolve_lan_url", lambda u, **kwargs: "http://203.0.113.9:1234/v1/chat/completions")
    translator = translate.LlamaTranslator(
        "madlad-3b", external_url="http://mac-studio.local:1234", external_model="m", external_api_key=KEY
    )

    with pytest.raises(ModelError) as refused:
        translator._complete(MESSAGES)
    assert str(refused.value) == KEY_ERROR_TR


def test_a_refused_key_is_not_swallowed_by_model_discovery(monkeypatch):
    forbid_requests(monkeypatch)
    translator = translate.LlamaTranslator(
        "madlad-3b", external_url="http://mac-studio.local:1234", external_api_key=KEY
    )
    translator._external_chat_url = "http://203.0.113.9:1234/v1/chat/completions"

    with pytest.raises(ModelError, match="API anahtarı"):
        translator._discover_external_model()


def test_the_key_is_not_sent_to_a_local_llama_server(monkeypatch):
    translator = translate.LlamaTranslator("gemma-4-e4b", completion_fn=lambda messages: "ok")
    translator._external_api_key = KEY  # e.g. left over in the environment while a local model is chosen

    assert "Authorization" not in translator._request_headers("http://127.0.0.1:8080/v1/chat/completions")


# ---- the `.local` subnet-scan fallback must never carry the key ----------------------------------------


def unresolvable_mdns(monkeypatch):
    def fail(*args, **kwargs):
        raise socket.gaierror("mDNS blocked")

    monkeypatch.setattr(translate.socket, "getaddrinfo", fail)


def test_scan_fallback_is_skipped_when_a_key_is_configured(monkeypatch):
    unresolvable_mdns(monkeypatch)
    monkeypatch.setattr(translate, "_discover_lan_server_ip", lambda port: pytest.fail("a scan would pick any host"))
    url = "http://mac-studio.local:1234/v1/chat/completions"

    assert translate._resolve_lan_url(url, allow_scan=False) == url


def test_scan_fallback_still_runs_without_a_key(monkeypatch):
    unresolvable_mdns(monkeypatch)
    scanned: list[int] = []
    monkeypatch.setattr(translate, "_discover_lan_server_ip", lambda port: scanned.append(port) or "192.168.1.77")

    resolved = translate._resolve_lan_url("http://mac-studio.local:1234/v1/chat/completions")

    assert resolved == "http://192.168.1.77:1234/v1/chat/completions" and scanned == [1234]


@pytest.mark.parametrize(("key", "allow_scan"), [(KEY, False), (None, True)])
def test_complete_asks_for_a_scan_only_without_a_key(monkeypatch, key, allow_scan):
    seen: list[bool] = []

    def resolve(url, allow_scan=True):
        seen.append(allow_scan)
        return url

    monkeypatch.setattr(translate, "_resolve_lan_url", resolve)
    recording_urlopen(monkeypatch)
    translator = translate.LlamaTranslator(
        "madlad-3b", external_url="http://mac-studio.local:1234", external_model="m", external_api_key=key
    )

    translator._complete(MESSAGES)

    assert seen == [allow_scan]


def test_a_failed_mdns_lookup_with_a_key_makes_no_probe_connection(monkeypatch):
    unresolvable_mdns(monkeypatch)
    monkeypatch.setattr(translate.socket, "create_connection", lambda *a, **k: pytest.fail("no probing with a key"))
    monkeypatch.setattr(translate.socket, "socket", lambda *a, **k: pytest.fail("no probing with a key"))
    requests = recording_urlopen(monkeypatch)
    translator = translate.LlamaTranslator(
        "madlad-3b", external_url="http://mac-studio.local:1234", external_model="m", external_api_key=KEY
    )

    translator._complete(MESSAGES)

    assert [r.full_url for r in requests] == ["http://mac-studio.local:1234/v1/chat/completions"]  # unchanged address


def test_probing_only_opens_connections_and_sends_no_bytes(monkeypatch):
    sent: list[bytes] = []

    class Connection:
        def __enter__(self):
            return self

        def __exit__(self, *args):
            return False

        def send(self, data, *args):
            sent.append(data)

        sendall = send

    class UdpSocket(Connection):
        def __init__(self, *args):
            pass

        def connect(self, address):
            pass

        def getsockname(self):
            return ("192.168.1.10", 50000)

    probed: list[str] = []

    def create_connection(address, timeout=None):
        probed.append(address[0])
        if address[0] != "192.168.1.77":
            raise OSError("closed")
        return Connection()

    monkeypatch.setattr(translate.socket, "socket", UdpSocket)
    monkeypatch.setattr(translate.socket, "create_connection", create_connection)
    forbid_requests(monkeypatch)

    assert translate._discover_lan_server_ip(1234) == "192.168.1.77"
    assert probed and "192.168.1.10" not in probed and sent == []


# ---- the key must not follow a redirect to another host ---------------------------------------------------


def redirect(handler_url: str, target: str, key: str | None = KEY, method: str = "GET", code: int = 302):
    request = urllib.request.Request(handler_url, headers={"Authorization": f"Bearer {key}"} if key else {}, method=method)
    handler = translate._KeySafeRedirectHandler()
    return handler.redirect_request(request, io.BytesIO(), code, "Found", {}, target)


@pytest.mark.parametrize("code", [301, 302, 303])
@pytest.mark.parametrize(
    "target",
    [
        "http://203.0.113.9/v1/models",
        "https://evil.example.com/v1/models",
        "http://192.168.1.99:1234/v1/models",  # another local host is still another host
        "http://localhost:1234/v1/models",  # same machine, but a different name
    ],
)
def test_redirect_to_another_host_drops_the_key(code, target):
    redirected = redirect("http://192.168.1.42:1234/v1/models", target, code=code)

    assert redirected is not None and redirected.full_url == target
    assert redirected.get_header("Authorization") is None


def test_redirect_to_another_host_keeps_other_headers():
    request = urllib.request.Request(
        "https://api.example.com/v1/models", headers={"Authorization": f"Bearer {KEY}", "Accept": "application/json"}
    )
    redirected = translate._KeySafeRedirectHandler().redirect_request(
        request, io.BytesIO(), 302, "Found", {}, "https://other.example.org/models"
    )

    assert redirected.get_header("Authorization") is None
    assert redirected.get_header("Accept") == "application/json"


@pytest.mark.parametrize(
    ("source", "target"),
    [
        ("http://192.168.1.42:1234/v1/models", "http://192.168.1.42:1234/v1/models/"),
        ("http://192.168.1.42:1234/v1/models", "http://192.168.1.42:8080/v1/models"),
        ("https://api.example.com/v1/models", "https://API.example.com/v2/models"),
        ("http://192.168.1.42/v1/models", "https://192.168.1.42/v1/models"),
    ],
)
def test_redirect_within_the_same_host_keeps_the_key(source, target):
    redirected = redirect(source, target)

    assert redirected.get_header("Authorization") == f"Bearer {KEY}"


def test_redirect_downgrading_to_plain_http_on_the_internet_drops_the_key():
    redirected = redirect("https://api.example.com/v1/models", "http://api.example.com/v1/models")

    assert redirected.get_header("Authorization") is None


@pytest.fixture
def real_http(monkeypatch):
    """Real sockets on loopback: a fresh process-wide opener, no proxy."""
    monkeypatch.setenv("NO_PROXY", "*")
    monkeypatch.setenv("no_proxy", "*")
    monkeypatch.setattr(urllib.request, "_opener", None)
    monkeypatch.setattr(translate, "_key_safe_opener_installed", False)


def serve(handler):
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, server.server_address[1]


def test_real_redirect_to_another_host_name_does_not_forward_the_key(real_http):
    seen: dict[str, str | None] = {}

    class Target(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            seen["target"] = self.headers.get("Authorization")
            body = json.dumps({"data": [{"id": "remote-model"}]}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    target, target_port = serve(Target)

    class Origin(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            seen["origin"] = self.headers.get("Authorization")
            self.send_response(302)
            self.send_header("Location", f"http://localhost:{target_port}/v1/models")  # same machine, other name
            self.send_header("Content-Length", "0")
            self.end_headers()

        def log_message(self, *args):
            pass

    origin, origin_port = serve(Origin)
    try:
        translator = translate.LlamaTranslator(
            "madlad-3b", external_url=f"http://127.0.0.1:{origin_port}", external_api_key=KEY
        )
        assert translator._discover_external_model() == "remote-model"  # the redirect was followed
    finally:
        origin.shutdown()
        target.shutdown()

    assert seen == {"origin": f"Bearer {KEY}", "target": None}


def test_real_redirect_within_the_same_host_keeps_the_key(real_http):
    seen: dict[str, str | None] = {}

    class Target(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            seen["target"] = self.headers.get("Authorization")
            body = json.dumps({"data": [{"id": "remote-model"}]}).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    target, target_port = serve(Target)

    class Origin(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(302)
            self.send_header("Location", f"http://127.0.0.1:{target_port}/v1/models")
            self.send_header("Content-Length", "0")
            self.end_headers()

        def log_message(self, *args):
            pass

    origin, origin_port = serve(Origin)
    try:
        translator = translate.LlamaTranslator(
            "madlad-3b", external_url=f"http://127.0.0.1:{origin_port}", external_api_key=KEY
        )
        assert translator._discover_external_model() == "remote-model"
    finally:
        origin.shutdown()
        target.shutdown()

    assert seen == {"target": f"Bearer {KEY}"}


def test_real_post_redirect_to_another_host_name_does_not_forward_the_key(real_http):
    seen: dict[str, str | None] = {}

    class Target(http.server.BaseHTTPRequestHandler):
        def do_GET(self):  # urllib turns a redirected POST (301/302/303) into a GET
            seen["target"] = self.headers.get("Authorization")
            body = json.dumps(CHAT_REPLY).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    target, target_port = serve(Target)

    class Origin(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            self.rfile.read(int(self.headers.get("Content-Length", "0")))
            seen["origin"] = self.headers.get("Authorization")
            self.send_response(303)
            self.send_header("Location", f"http://localhost:{target_port}/v1/chat/completions")
            self.send_header("Content-Length", "0")
            self.end_headers()

        def log_message(self, *args):
            pass

    origin, origin_port = serve(Origin)
    try:
        translator = translate.LlamaTranslator(
            "madlad-3b", external_url=f"http://127.0.0.1:{origin_port}", external_model="m", external_api_key=KEY
        )
        assert translator._complete(MESSAGES) == "[1] Merhaba!"
    finally:
        origin.shutdown()
        target.shutdown()

    assert seen == {"origin": f"Bearer {KEY}", "target": None}


# ---- strict offline mode blocks servers outside the local network --------------------------------------

CLOUD_URLS = [
    ("https://api.openai.com/v1", "api.openai.com"),
    ("https://opencode.ai/zen/go/v1", "opencode.ai"),
    ("https://openrouter.ai/api/v1", "openrouter.ai"),
    ("https://generativelanguage.googleapis.com/v1beta/openai", "generativelanguage.googleapis.com"),
    ("http://203.0.113.9:1234", "203.0.113.9"),
    ("api.deepseek.com", "api.deepseek.com"),
]
LAN_URLS = [
    "192.168.1.42:1234",
    "http://10.0.0.5:1234",
    "http://172.20.1.1",
    "http://100.101.102.103:8080",
    "http://169.254.1.2:1234",
    "http://[fd00::1]:1234",
    "http://[fe80::1]:1234",
    "http://127.0.0.1:1234",
    "http://localhost:11434",
    "http://mac-studio.local:1234",
    "http://ollama:11434",
]


@pytest.mark.parametrize(("url", "host"), CLOUD_URLS)
def test_offline_mode_blocks_an_internet_server_before_any_request(monkeypatch, url, host):
    forbid_requests(monkeypatch)
    monkeypatch.setenv(models.OFFLINE_ENV_VAR, "1")

    with pytest.raises(ModelError) as tr:
        translate.LlamaTranslator("madlad-3b", external_url=url, external_model="m")
    assert str(tr.value) == (
        f"Çevrimdışı mod açık: çeviri internetteki bir sağlayıcıya ({host}) gönderilemez. "
        "Çevrimdışı modu kapatın ya da yerel bir model/sunucu seçin."
    )

    monkeypatch.setenv(i18n.ENV_VAR, "en")
    with pytest.raises(ModelError) as en:
        translate.LlamaTranslator("madlad-3b", external_url=url, external_model="m")
    assert str(en.value) == (
        f"Offline mode is on: translation can't be sent to an internet provider ({host}). "
        "Turn offline mode off or choose a local model/server."
    )


@pytest.mark.parametrize("flag", ["env", "cli"])
def test_offline_mode_blocks_an_internet_server_through_the_cli_before_any_work(monkeypatch, capsys, flag):
    forbid_requests(monkeypatch)
    monkeypatch.setattr(models, "ensure_model", lambda *a, **k: pytest.fail("must not download"))
    monkeypatch.setattr(translate, "create_translator", lambda *a, **k: pytest.fail("must not load anything"))
    monkeypatch.setenv(translate.EXTERNAL_LLM_URL_ENV_VAR, "https://api.openai.com/v1")
    monkeypatch.setenv(translate.EXTERNAL_LLM_API_KEY_ENV_VAR, KEY)
    monkeypatch.setattr(sys, "stdin", io.StringIO("[]"))
    args = ["translate", "--to", "tr", "--json"]
    if flag == "env":
        monkeypatch.setenv(models.OFFLINE_ENV_VAR, "1")
    else:
        args = ["--offline", *args]

    assert cli.main(args) == 1

    out = capsys.readouterr().out
    event = json.loads(out)  # a single line: no stage event came first
    assert event["event"] == "error" and "Çevrimdışı mod açık" in event["message"] and "(api.openai.com)" in event["message"]
    assert KEY not in out


@pytest.mark.parametrize("url", LAN_URLS)
def test_offline_mode_allows_servers_on_the_local_network(monkeypatch, url):
    monkeypatch.setenv(models.OFFLINE_ENV_VAR, "1")
    monkeypatch.setattr(translate, "_resolve_lan_url", lambda u, **kwargs: u)
    requests = recording_urlopen(monkeypatch)
    translator = translate.LlamaTranslator("madlad-3b", external_url=url, external_model="m")

    assert translator._complete(MESSAGES) == "[1] Merhaba!"
    assert len(requests) == 1


def test_offline_mode_allows_a_local_name_however_it_resolves(monkeypatch):
    """The `.local` name is what counts; the address mDNS or the subnet scan finds later is not re-judged."""
    monkeypatch.setenv(models.OFFLINE_ENV_VAR, "1")
    unresolvable_mdns(monkeypatch)
    monkeypatch.setattr(translate, "_discover_lan_server_ip", lambda port: "192.168.1.77")
    requests = recording_urlopen(monkeypatch)
    translator = translate.LlamaTranslator("madlad-3b", external_url="http://mac-studio.local:1234", external_model="m")

    translator._complete(MESSAGES)

    assert requests[0].full_url == "http://192.168.1.77:1234/v1/chat/completions"


def test_offline_mode_allows_a_lan_server_through_the_cli(monkeypatch, capsys):
    monkeypatch.setenv(models.OFFLINE_ENV_VAR, "1")
    monkeypatch.setenv(translate.EXTERNAL_LLM_URL_ENV_VAR, "http://192.168.1.42:1234")
    monkeypatch.setenv(translate.EXTERNAL_LLM_MODEL_ENV_VAR, "qwen3")
    monkeypatch.setattr(translate, "_resolve_lan_url", lambda u, **kwargs: u)
    monkeypatch.setattr(translate, "_temp_checkpoint_path", lambda *a, **kw: None)
    requests = recording_urlopen(monkeypatch)
    payload = [{"id": 1, "start": 0.0, "end": 2.0, "text": "Hello!", "words": []}]
    monkeypatch.setattr(sys, "stdin", io.StringIO(json.dumps(payload)))

    assert cli.main(["translate", "--to", "tr", "--from", "en", "--json"]) == 0

    events = [json.loads(line) for line in capsys.readouterr().out.splitlines()]
    assert events[-1]["event"] == "translation" and events[-1]["sentences"][0]["text"] == "Merhaba!"
    assert requests and requests[0].full_url == "http://192.168.1.42:1234/v1/chat/completions"


@pytest.mark.parametrize(("url", "host"), CLOUD_URLS)
def test_online_mode_allows_a_cloud_server(monkeypatch, url, host):
    requests = recording_urlopen(monkeypatch)
    translator = translate.LlamaTranslator("madlad-3b", external_url=url, external_model="m")

    assert translator._complete(MESSAGES) == "[1] Merhaba!"
    assert len(requests) == 1


@pytest.mark.parametrize("value", ["", "0", "false", "no", "off"])
def test_offline_flags_that_are_off_do_not_block(monkeypatch, value):
    monkeypatch.setenv(models.OFFLINE_ENV_VAR, value)

    translate.check_external_llm("https://api.openai.com/v1")


def test_offline_rule_is_checked_before_the_key_rule(monkeypatch):
    monkeypatch.setenv(models.OFFLINE_ENV_VAR, "1")

    with pytest.raises(ModelError, match="Çevrimdışı mod açık"):
        translate.check_external_llm("http://api.openai.com/v1", KEY)


def test_check_external_llm_returns_the_chat_url():
    assert translate.check_external_llm("192.168.1.42:1234") == "http://192.168.1.42:1234/v1/chat/completions"
    assert translate.check_external_llm("https://openrouter.ai/api/v1", KEY).endswith("/api/v1/chat/completions")


def test_a_hostless_address_still_gets_the_address_error():
    with pytest.raises(ModelError, match="Harici LLM sunucusu adresi boş olamaz."):
        translate.check_external_llm("http://", KEY)

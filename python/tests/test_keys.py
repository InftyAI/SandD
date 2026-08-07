"""Unit tests for the key-broker helpers (sandd.keys).

A real loopback HTTP server stands in for the broker rather than a mock of
urllib: the helper's job is exactly the HTTP request/parse, so mocking that away
would test nothing. Each test drives a handler that records what it received.
"""

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest
from sandd import tunnel_config_from_env
from sandd.keys import KEYBROKER_URL_ENV, TUNNEL_SERVER_ENV, mint_authkey


class _Broker:
    """A stub key broker on loopback. `responses` is a list of (status, body)
    consumed one per request, so retry behaviour can be scripted."""

    def __init__(self, responses):
        self.responses = list(responses)
        self.requests = []  # (method, path) per received request

        broker = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                broker.requests.append(("POST", self.path))
                broker._respond(self)

            def do_GET(self):
                broker.requests.append(("GET", self.path))
                broker._respond(self)

            def log_message(self, *args):
                pass  # keep pytest output clean

        self._server = HTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self._server.server_port}"
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    def _respond(self, handler):
        status, body = (
            self.responses.pop(0) if self.responses else (500, "no scripted response")
        )
        payload = body.encode() if isinstance(body, str) else json.dumps(body).encode()
        handler.send_response(status)
        handler.send_header("Content-Type", "application/json")
        handler.send_header("Content-Length", str(len(payload)))
        handler.end_headers()
        handler.wfile.write(payload)

    def __enter__(self):
        self._thread.start()
        return self

    def __exit__(self, *exc):
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)


class TestMintAuthkey:
    """Test mint_authkey"""

    def test_returns_key(self):
        """A 200 with a key yields that key"""
        with _Broker([(200, {"key": "nodekey-abc"})]) as broker:
            assert mint_authkey(broker_url=broker.url) == "nodekey-abc"

    def test_posts_to_keys_with_kind(self):
        """The broker is called with POST /keys?kind=<kind>, no doubled slash"""
        with _Broker([(200, {"key": "k"})]) as broker:
            mint_authkey(kind="daemon", broker_url=broker.url + "/")
        assert broker.requests == [("POST", "/keys?kind=daemon")]

    def test_defaults_to_controller_kind(self):
        """kind defaults to controller — the SDK's caller is the controller"""
        with _Broker([(200, {"key": "k"})]) as broker:
            mint_authkey(broker_url=broker.url)
        assert broker.requests == [("POST", "/keys?kind=controller")]

    def test_reads_broker_url_from_env(self, monkeypatch):
        """With no broker_url, $SANDD_KEYBROKER_URL is used"""
        with _Broker([(200, {"key": "from-env"})]) as broker:
            monkeypatch.setenv(KEYBROKER_URL_ENV, broker.url)
            assert mint_authkey() == "from-env"

    def test_explicit_url_wins_over_env(self, monkeypatch):
        with _Broker([(200, {"key": "explicit"})]) as broker:
            monkeypatch.setenv(KEYBROKER_URL_ENV, "http://127.0.0.1:1/unused")
            assert mint_authkey(broker_url=broker.url) == "explicit"

    def test_missing_url_raises(self, monkeypatch):
        monkeypatch.delenv(KEYBROKER_URL_ENV, raising=False)
        with pytest.raises(ValueError, match=KEYBROKER_URL_ENV):
            mint_authkey()

    def test_blank_url_raises(self, monkeypatch):
        monkeypatch.setenv(KEYBROKER_URL_ENV, "   ")
        with pytest.raises(ValueError, match=KEYBROKER_URL_ENV):
            mint_authkey()

    def test_retries_then_succeeds(self):
        """A transient 502 (broker's own headscale call failed) is retried.

        This is the startup race the broker sidecar actually exhibits, so the
        retry must be real, not decorative.
        """
        with _Broker([(502, "failed to mint key"), (200, {"key": "second-try"})]) as b:
            assert mint_authkey(broker_url=b.url, attempts=2) == "second-try"
            assert len(b.requests) == 2

    def test_exhausted_attempts_raise(self):
        with _Broker([(502, "nope"), (502, "nope")]) as broker:
            with pytest.raises(RuntimeError, match="after 2 attempt"):
                mint_authkey(broker_url=broker.url, attempts=2)
            assert len(broker.requests) == 2

    def test_empty_key_raises(self):
        """A 200 whose body has no key is a failure, not an empty key"""
        with _Broker([(200, {"key": ""})]) as broker:
            with pytest.raises(RuntimeError):
                mint_authkey(broker_url=broker.url, attempts=1)

    def test_malformed_body_raises(self):
        with _Broker([(200, "not json")]) as broker:
            with pytest.raises(RuntimeError):
                mint_authkey(broker_url=broker.url, attempts=1)

    def test_unreachable_broker_raises(self):
        """Port 1 on loopback refuses connections — no server needed"""
        with pytest.raises(RuntimeError):
            mint_authkey(broker_url="http://127.0.0.1:1", attempts=1)

    def test_zero_attempts_rejected(self):
        with pytest.raises(ValueError, match="attempts"):
            mint_authkey(broker_url="http://127.0.0.1:1", attempts=0)

    def test_key_never_appears_in_error(self):
        """Keys are secrets: the failure path must not echo the body.

        A 200 with a key under the wrong field name is the nastiest case — the
        body IS key material and the helper still has to fail.
        """
        with _Broker([(200, {"authkey": "SECRET-KEY-MATERIAL"})]) as broker:
            with pytest.raises(RuntimeError) as excinfo:
                mint_authkey(broker_url=broker.url, attempts=1)
            assert "SECRET-KEY-MATERIAL" not in str(excinfo.value)


class TestTunnelConfigFromEnv:
    """Test tunnel_config_from_env"""

    def test_builds_config(self, monkeypatch):
        with _Broker([(200, {"key": "minted"})]) as broker:
            monkeypatch.setenv(KEYBROKER_URL_ENV, broker.url)
            monkeypatch.setenv(TUNNEL_SERVER_ENV, "http://headscale.test")
            config = tunnel_config_from_env()
        assert config.authkey == "minted"
        assert config.server == "http://headscale.test"

    def test_missing_tunnel_server_raises(self, monkeypatch):
        """Fails BEFORE minting: no point burning a key we can't use"""
        with _Broker([(200, {"key": "unused"})]) as broker:
            monkeypatch.setenv(KEYBROKER_URL_ENV, broker.url)
            monkeypatch.delenv(TUNNEL_SERVER_ENV, raising=False)
            with pytest.raises(ValueError, match=TUNNEL_SERVER_ENV):
                tunnel_config_from_env()
            assert broker.requests == []

    def test_explicit_server_wins_over_env(self, monkeypatch):
        with _Broker([(200, {"key": "k"})]) as broker:
            monkeypatch.setenv(KEYBROKER_URL_ENV, broker.url)
            monkeypatch.setenv(TUNNEL_SERVER_ENV, "http://from-env")
            config = tunnel_config_from_env(server="http://explicit")
        assert config.server == "http://explicit"

    def test_explicit_server_without_env(self, monkeypatch):
        """server= alone is enough — $SANDD_TUNNEL_SERVER need not be set"""
        with _Broker([(200, {"key": "k"})]) as broker:
            monkeypatch.delenv(TUNNEL_SERVER_ENV, raising=False)
            config = tunnel_config_from_env(
                server="http://headscale.test", broker_url=broker.url
            )
        assert config.server == "http://headscale.test"

    def test_blank_server_raises(self, monkeypatch):
        with _Broker([(200, {"key": "unused"})]) as broker:
            monkeypatch.setenv(KEYBROKER_URL_ENV, broker.url)
            with pytest.raises(ValueError, match=TUNNEL_SERVER_ENV):
                tunnel_config_from_env(server="   ")
            assert broker.requests == []

    def test_forwards_kwargs_to_mint(self, monkeypatch):
        """kind/attempts reach mint_authkey rather than being silently dropped"""
        with _Broker([(502, "x"), (200, {"key": "k"})]) as broker:
            monkeypatch.setenv(TUNNEL_SERVER_ENV, "http://headscale.test")
            config = tunnel_config_from_env(
                kind="daemon", broker_url=broker.url, attempts=2
            )
        assert config.authkey == "k"
        assert broker.requests == [
            ("POST", "/keys?kind=daemon"),
            ("POST", "/keys?kind=daemon"),
        ]

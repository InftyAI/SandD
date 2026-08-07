"""Mesh auth-key helpers for tunnel mode.

A controller in tunnel mode needs a headscale pre-auth key before it can join the
mesh. In a Nebula cluster that key is NOT a static secret: an in-cluster key broker
mints a fresh reusable+ephemeral one per caller, and the broker is the only component
holding headscale admin authority. Every controller therefore had to hand-roll the
same POST-and-parse against the broker before constructing a ``Server`` — see the
`sandd-controller` sample in the Nebula repo, which carried it inline. That
boilerplate lives here instead.

Only the stdlib is used (``urllib``): the package declares no runtime dependencies,
and a controller image that must talk to the broker before it can do anything else
is the wrong place to require ``requests``.

Keys are secrets. Nothing here logs, prints, or embeds a key in an exception
message — including on the error paths, where the HTTP status is the actionable
signal and the body may still be key material.
"""

import json
import os
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Optional

try:
    from ._core import TunnelConfig
except ImportError as e:  # pragma: no cover - mirrors server.py's import guard
    raise ImportError(
        "Failed to import Rust extension. Please build the package with: make install"
    ) from e

__all__ = ["mint_authkey", "tunnel_config_from_env"]

#: Env var holding the key-broker root URL, e.g.
#: ``http://nebula-keybroker.nebula-system:8090``.
KEYBROKER_URL_ENV = "SANDD_KEYBROKER_URL"

#: Env var holding the headscale URL the controller joins, e.g.
#: ``http://nebula-headscale.nebula-system``. Consumed by
#: :func:`tunnel_config_from_env` only.
TUNNEL_SERVER_ENV = "SANDD_TUNNEL_SERVER"


def mint_authkey(
    kind: str = "controller",
    broker_url: Optional[str] = None,
    timeout: float = 10.0,
    attempts: int = 3,
) -> str:
    """Mint a fresh headscale pre-auth key from the key broker.

    Args:
        kind: Key policy to request, "controller" or "daemon". The broker owns the
            policy (reusability, ephemerality, expiry); the caller only names a role.
        broker_url: Broker root URL. Defaults to ``$SANDD_KEYBROKER_URL``.
        timeout: Per-attempt timeout in seconds. A healthy mint is sub-second — the
            broker shells out to a local CLI — so this bounds a wedged broker rather
            than a slow one.
        attempts: Total tries, with 1s/2s/... backoff between them. Defaults to 3
            because the broker commonly runs as a headscale sidecar and its socket
            may not be up yet when a controller starts; a startup race should not
            crash-loop the pod.

    Returns:
        The key. It is a secret — do not log or print it.

    Raises:
        ValueError: If no broker URL was given or found in the environment.
        RuntimeError: If every attempt failed, or the broker returned no key.
    """
    if attempts < 1:
        raise ValueError(f"attempts must be >= 1, got {attempts}")

    base = broker_url if broker_url is not None else os.environ.get(KEYBROKER_URL_ENV)
    if not base or not base.strip():
        raise ValueError(
            "no key-broker URL: pass broker_url= or set "
            f"${KEYBROKER_URL_ENV} (e.g. http://nebula-keybroker.nebula-system:8090)"
        )

    url = f"{base.strip().rstrip('/')}/keys?kind={urllib.parse.quote(kind)}"

    last_error = None
    for attempt in range(attempts):
        if attempt:
            time.sleep(float(attempt))
        try:
            return _mint_once(url, timeout)
        except (urllib.error.URLError, OSError, ValueError) as e:
            # URLError covers HTTPError (a 4xx/5xx from the broker) and connection
            # failures alike; both are worth retrying, since a 502 here means the
            # broker's own call to headscale failed transiently. ValueError is a
            # malformed/empty response body.
            last_error = e

    raise RuntimeError(
        f"minting {kind} key from key broker failed after {attempts} attempt(s): {last_error}"
    )


def _mint_once(url: str, timeout: float) -> str:
    """POST once and return the key, or raise for the caller to retry."""
    req = urllib.request.Request(url, method="POST")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        # Never surface the body in an error: on success it is key material.
        payload = json.load(resp)
    key = payload.get("key") if isinstance(payload, dict) else None
    if not key:
        raise ValueError("key broker returned an empty key")
    return key


def tunnel_config_from_env(
    kind: str = "controller", server: Optional[str] = None, **kwargs
) -> TunnelConfig:
    """Build a :class:`TunnelConfig` from the environment, minting the auth key.

    Reads the headscale URL from ``$SANDD_TUNNEL_SERVER`` and mints a key via
    :func:`mint_authkey`, so a controller needs no key handling of its own:

        >>> from sandd import Server, tunnel_config_from_env
        >>> server = Server(connect="tunnel", tunnel_config=tunnel_config_from_env())

    Args:
        kind: Passed through to :func:`mint_authkey`.
        server: headscale URL to join. Defaults to ``$SANDD_TUNNEL_SERVER``, which
            is how it is set in-cluster; pass it explicitly to override, mirroring
            ``broker_url``.
        **kwargs: Passed through to :func:`mint_authkey` (``broker_url``,
            ``timeout``, ``attempts``).

    Raises:
        ValueError: If no headscale URL was given or found in the environment, or
            the broker URL is missing.
        RuntimeError: If minting failed.
    """
    if server is None:
        server = os.environ.get(TUNNEL_SERVER_ENV)
    if not server or not server.strip():
        raise ValueError(
            f"no headscale URL: pass server= or set ${TUNNEL_SERVER_ENV} "
            "(e.g. http://nebula-headscale.nebula-system)"
        )
    return TunnelConfig(authkey=mint_authkey(kind, **kwargs), server=server.strip())

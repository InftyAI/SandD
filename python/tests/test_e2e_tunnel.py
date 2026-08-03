"""Tunnel-mode E2E — regression test for the userspace-networking dial path.

Run with: make test-e2e-tunnel

WHY A SEPARATE FILE FROM test_e2e.py:
    test_e2e.py is DIRECT mode: daemons reach the host's :8765 over the Docker
    bridge. That (and examples/tunnel-simple) let a plain socket carry the
    connection, so the tailnet was set up but never load-bearing. The path Nebula
    uses in prod — an UNPRIVILEGED daemon in --tun=userspace-networking, where the
    tailnet has no kernel route and the WebSocket must go THROUGH tailscaled's
    SOCKS5 proxy — had zero coverage. This test exercises exactly that path.

HOW IT FORCES THE MESH (see docker-compose.tunnel-e2e.yml for the full rationale):
    The daemon dials the controller by its MagicDNS name (controller.sandd.local),
    which the Docker bridge cannot resolve — only tailscaled can. Combined with the
    daemon being unprivileged/userspace-networking, the ONLY way it connects is via
    the SOCKS5 proxy. Pre-fix, the daemon joins the mesh but never connects, and
    this test fails; post-fix it connects and exec works.

The controller runs INSIDE a container (it needs a real TUN, awkward for a host
Python process), so we assert on the controller's log markers rather than driving
a Server() from the test process.
"""

import os
import subprocess
import time

import pytest

# Both markers: `e2e` (needs Docker, skipped in `make test`) and `tunnel` (needs
# the dedicated headscale/mesh compose stack — selected/excluded on its own).
pytestmark = [pytest.mark.e2e, pytest.mark.tunnel]

COMPOSE_FILE = "hack/docker/docker-compose.tunnel-e2e.yml"
DAEMON_ID = "tunnel-daemon-1"
# Tunnel bring-up (tailscaled start + mesh join on two nodes + DERP negotiation)
# is much slower than direct mode; give it generous headroom.
CONNECT_TIMEOUT_S = 180
POLL_INTERVAL_S = 3


def _compose(*args, check=True, capture=True):
    return subprocess.run(
        ["docker", "compose", "-f", COMPOSE_FILE, *args],
        check=check,
        capture_output=capture,
        text=True,
    )


def _headscale_container() -> str:
    """Resolve the headscale container id (compose name is project-prefixed)."""
    out = _compose("ps", "-q", "headscale").stdout.strip()
    assert out, "headscale container not found — did `compose up headscale` run?"
    return out.splitlines()[0]


def _controller_logs() -> str:
    # check=False: the controller may still be starting; empty logs are fine.
    return _compose("logs", "controller", check=False).stdout


@pytest.fixture(scope="module")
def tunnel_stack():
    """Bring up headscale, mint a reusable auth key, then start controller+daemon.

    The auth key must exist BEFORE the controller/daemon start (they consume it via
    SANDD_TUNNEL_AUTH_KEY to join the mesh), so this mirrors the tunnel-simple
    bring-up order: headscale first, mint key, then the rest.
    """
    _compose("build")

    # 1. headscale only.
    _compose("up", "-d", "headscale")
    time.sleep(3)

    # 2. mint a reusable pre-auth key (controller + daemon share it in this test).
    hs = _headscale_container()
    subprocess.run(
        ["docker", "exec", hs, "headscale", "users", "create", "sandd"],
        check=False,  # idempotent-ish: fine if the user already exists on rerun
        capture_output=True,
        text=True,
    )
    key = subprocess.run(
        [
            "docker", "exec", hs, "headscale", "preauthkeys", "create",
            "--user", "sandd", "--reusable", "--expiration", "1h",
        ],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip().splitlines()[-1].strip()
    assert key and " " not in key, f"unexpected preauthkey output: {key!r}"

    # 3. controller + daemon, with the freshly-minted key in their env. Compose
    #    substitutes ${SANDD_TUNNEL_AUTH_KEY} from this process's environment.
    subprocess.run(
        ["docker", "compose", "-f", COMPOSE_FILE, "up", "-d", "controller", "daemon"],
        check=True,
        capture_output=True,
        text=True,
        env={**os.environ, "SANDD_TUNNEL_AUTH_KEY": key},
    )

    yield

    _compose("down", "-v", check=False)


def _wait_for_marker(marker: str, timeout: float) -> str:
    """Poll the controller logs until `marker` appears; return the logs seen."""
    deadline = time.time() + timeout
    logs = ""
    while time.time() < deadline:
        logs = _controller_logs()
        if marker in logs:
            return logs
        time.sleep(POLL_INTERVAL_S)
    return logs


class TestE2ETunnel:
    """The daemon must connect and be usable over the mesh, unprivileged."""

    def test_daemon_connects_over_mesh(self, tunnel_stack):
        """Daemon joins the mesh and its WebSocket reaches the controller.

        This is THE regression assertion: it can only pass if the daemon's
        WebSocket traverses tailscaled's SOCKS5 proxy (userspace-networking has no
        kernel route to the controller's mesh IP, and the MagicDNS name is
        unresolvable on the Docker bridge).
        """
        marker = f"DAEMON_CONNECTED {DAEMON_ID}"
        logs = _wait_for_marker(marker, CONNECT_TIMEOUT_S)
        if marker not in logs:
            # Surface daemon logs too — a mesh-join vs SOCKS-dial failure shows here.
            daemon_logs = _compose("logs", "daemon", check=False).stdout
            pytest.fail(
                "daemon never connected to the controller over the mesh within "
                f"{CONNECT_TIMEOUT_S}s (SOCKS5 tunnel path broken?).\n"
                f"--- controller logs ---\n{logs[-3000:]}\n"
                f"--- daemon logs ---\n{daemon_logs[-3000:]}"
            )

    def test_exec_over_mesh(self, tunnel_stack):
        """A command runs on the daemon and its output returns over the mesh."""
        marker = f"EXEC_OK {DAEMON_ID}"
        logs = _wait_for_marker(marker, CONNECT_TIMEOUT_S)
        assert marker in logs, (
            "connected but exec over the mesh did not succeed; last controller "
            f"logs:\n{logs[-3000:]}"
        )

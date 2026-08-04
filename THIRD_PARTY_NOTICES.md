# Third-party notices

SandD is licensed under Apache-2.0 (see [LICENSE](./LICENSE)). It relies on the
following third-party software, which is licensed separately.

## Tailscale

Tunnel mode uses the [Tailscale](https://github.com/tailscale/tailscale) client
(`tailscale`/`tailscaled`), © Tailscale Inc., licensed under
[BSD-3-Clause](https://github.com/tailscale/tailscale/blob/main/LICENSE).

SandD invokes it as a separate process and fetches it at runtime (the install script
and tunnel Dockerfiles pull the official binaries) — it is not linked as a library or
included in SandD's source. If you build and redistribute an image with Tailscale
baked in, retain its copyright notice per BSD-3-Clause.

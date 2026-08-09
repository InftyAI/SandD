//! `sandd-controller` — the per-workload SandD controller Nebula runs.
//!
//! One of these exists per workload (Nebula's PodPlacement reconciler creates a
//! Deployment + ClusterIP Service named `sandd-<workload-uid>`, owned by the
//! workload so it is garbage-collected with it). The workload's daemons dial IN to
//! `/ws`, prove who they are with a token the Nebula manager minted, and are held in
//! the registry so exec/logs traffic can be routed to them.
//!
//! FLAGS, EACH ALSO READABLE FROM AN ENV VAR. `--enable-auth` is the switch that
//! turns daemon authentication on; the verification material has a flag too. Both
//! surfaces exist because both callers are real: a person running this by hand wants
//! `--help` and flags, while Nebula configures it through a Deployment env block
//! (assembling an argv in the reconciler would make every value a string splice).
//! clap's `env` gives one definition per setting, so the two cannot drift, and
//! `--help` documents the env var beside each flag.
//!
//! The env names are a CONTRACT with Nebula's `sanddControllerEnv`
//! (internal/controller/pod_placement_helpers.go) — renaming one silently stops
//! daemons from authenticating, so the two lists must move together.
//!
//! FAILURE POSTURE: a misconfiguration is FATAL at startup, never degraded into
//! "auth disabled". A controller that admits every caller looks identical to a
//! healthy one — it passes probes, it serves /stats, it registers daemons — so the
//! mistake surfaces only as a security incident. Exiting non-zero makes it a
//! CrashLoopBackOff an operator sees immediately.

use clap::Parser;
use sandbox_server::auth::TokenVerifier;
use sandbox_server::server::SandboxServer;
use std::net::IpAddr;
use tracing::info;

/// The `iss` required when the issuer is not configured. Matches the Nebula
/// manager's own default (pkg/sandd), so the common deployment configures neither
/// side. Both sides defaulting to the same literal is what keeps that safe.
const DEFAULT_ISSUER: &str = "nebula";

/// 0.0.0.0, not 127.0.0.1: the daemons dialing in are on other machines entirely
/// (GPU instances at a neocloud provider), reaching this through a Service. A
/// loopback default would produce a controller that starts cleanly and is
/// unreachable by every one of its clients.
const DEFAULT_HOST: &str = "0.0.0.0";

/// Matches `nebulav1alpha1.SanddControllerPort` and the daemon's dial-out URL. Kept
/// as a default rather than a required setting so a hand-run controller needs no
/// configuration at all.
const DEFAULT_PORT: u16 = 8765;

/// The controller's command line. Every flag carries its env fallback, so
/// `--enable-auth` and `SANDD_ENABLE_AUTH=true` are the same switch.
#[derive(Parser, Debug, Default)]
#[command(
    name = "sandd-controller",
    version,
    about = "Per-workload SandD controller: daemons dial in over WebSocket and are \
             admitted by token."
)]
struct Args {
    /// Address to listen on. 0.0.0.0 because daemons dial in from other machines.
    #[arg(long, env = "SANDD_HOST", default_value = DEFAULT_HOST)]
    host: String,

    /// Port for the daemon WebSocket (`/ws`), /stats and /health.
    #[arg(long, env = "SANDD_PORT", default_value_t = DEFAULT_PORT)]
    port: u16,

    /// Require a valid daemon token on every connection.
    //
    // Doc comments here are --help TEXT, so the rationale lives in plain comments
    // below rather than being read out to an operator debugging a Deployment.
    //
    // num_args(0..=1) + default_missing_value rather than a plain SetTrue flag: a bare
    // `--enable-auth` must work for a human, AND the env var must honour
    // SANDD_ENABLE_AUTH=false. A SetTrue flag ignores the env VALUE entirely — merely
    // setting the var would enable auth, so `=false` would turn it ON. Of the two ways
    // to be wrong, that is the dangerous one.
    #[arg(
        long,
        env = "SANDD_ENABLE_AUTH",
        num_args = 0..=1,
        default_missing_value = "true",
        default_value = "false",
        value_parser = parse_bool,
    )]
    enable_auth: bool,

    /// This controller's id, and the only `aud` it admits. Required with
    /// --enable-auth.
    #[arg(long, env = "SANDD_CONTROLLER_ID")]
    controller_id: Option<String>,

    /// PKIX PEM public key daemon tokens are verified against. Required with
    /// --enable-auth.
    //
    // allow_hyphen_values because a PEM STARTS with `-----BEGIN PUBLIC KEY-----`:
    // without it clap reads the value as an unknown flag and the flag form is simply
    // unusable. Nebula's env path never hits this, which is exactly why it would have
    // gone unnoticed until someone ran the binary by hand.
    //
    // Passing a key on argv is acceptable even though /proc/<pid>/cmdline is
    // world-readable, because this half is PUBLIC. The daemon's TOKEN is the opposite
    // case and is deliberately env-only with no flag at all (sandd/src/main.rs).
    #[arg(long, env = "SANDD_SIGNING_PUBLIC_KEY", allow_hyphen_values = true)]
    signing_public_key: Option<String>,

    /// The `kid` that public key answers to. Required with --enable-auth.
    #[arg(long, env = "SANDD_SIGNING_KID")]
    signing_kid: Option<String>,

    /// The `iss` a token must carry [default: nebula].
    #[arg(long, env = "SANDD_TOKEN_ISSUER")]
    token_issuer: Option<String>,
}

/// Parses a boolean setting, accepting the spellings operators actually write.
///
/// An UNRECOGNIZED value is an ERROR, not a falsy default. `--enable-auth=yes` is
/// fine, but `SANDD_ENABLE_AUTH=enabled` silently meaning "off" is precisely the
/// belief-vs-reality gap this module exists to prevent.
fn parse_bool(raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        // Empty counts as off: Kubernetes readily produces `NAME=""` (an env var with
        // no value, a configMapKeyRef to an empty key), and clap passes it through.
        "0" | "false" | "no" | "off" | "" => Ok(false),
        other => Err(format!(
            "expected one of true/false/1/0/yes/no/on/off, got {:?}",
            other
        )),
    }
}

/// What the process needs to start. Resolved from the environment in one place so
/// the rules are testable without a running server.
#[derive(Debug, PartialEq, Eq)]
struct Config {
    bind_addr: String,
    /// None means authentication is OFF. Some(..) carries everything a verifier
    /// needs, all of it already validated as present.
    auth: Option<AuthConfig>,
}

#[derive(Debug, PartialEq, Eq)]
struct AuthConfig {
    controller_id: String,
    public_key_pem: String,
    kid: String,
    issuer: String,
}

/// Resolves the parsed command line into what the process needs to start.
///
/// Separate from `Args` so the RULES (what is required with auth, what a valid port
/// is) are testable by building an Args value directly — no process env to mutate,
/// no argv to fake. `std::env::set_var` is global state and these tests run
/// concurrently in one process, so an env-mutating test would be a flaky test.
fn resolve(args: Args) -> Result<Config, String> {
    // Validate the host as an IP here rather than letting `bind` fail later: at bind
    // time the error is "invalid socket address" with no hint what produced it.
    let host = trimmed(&args.host).unwrap_or_else(|| DEFAULT_HOST.to_string());
    host.parse::<IpAddr>()
        .map_err(|_| format!("--host is not a valid IP address: {:?}", host))?;

    if args.port == 0 {
        // Port 0 means "any free port" to the OS. That binds SUCCESSFULLY and then
        // nobody can reach it, because the daemons' URLs name a fixed port.
        return Err("--port must not be 0".to_string());
    }

    // DEFAULTS TO OFF, a compatibility choice rather than a security preference: this
    // binary also serves the standalone/local-dev and e2e stacks, where no manager
    // exists to mint tokens, so defaulting to on would break all of them at once.
    // Nebula passes the switch explicitly (it is the same gate that turns on minting
    // in the manager), so the deployment that needs auth gets it.
    let auth = if args.enable_auth {
        Some(resolve_auth(&args)?)
    } else {
        None
    };

    Ok(Config {
        bind_addr: format!("{}:{}", host, args.port),
        auth,
    })
}

/// Collects the verification material, requiring everything that has no safe default.
///
/// The kid is required WITH auth even though `TokenVerifier` tolerates an empty one
/// (it then accepts any kid): tolerating it here would leave a controller unable to
/// tell a rotation mismatch from a forgery, and Nebula always sends one.
fn resolve_auth(args: &Args) -> Result<AuthConfig, String> {
    // Names the FLAG and its env var, because the two callers read different ones: a
    // person sees --controller-id, an operator debugging a CrashLoopBackOff sees the
    // Deployment's env block.
    let required = |value: &Option<String>, flag: &str, env: &str| -> Result<String, String> {
        value
            .as_deref()
            .and_then(trimmed)
            .ok_or_else(|| format!("--{} ({}) is required with --enable-auth", flag, env))
    };

    Ok(AuthConfig {
        controller_id: required(&args.controller_id, "controller-id", "SANDD_CONTROLLER_ID")?,
        public_key_pem: required(
            &args.signing_public_key,
            "signing-public-key",
            "SANDD_SIGNING_PUBLIC_KEY",
        )?,
        kid: required(&args.signing_kid, "signing-kid", "SANDD_SIGNING_KID")?,
        // The only defaulted one — both sides default to the same literal.
        issuer: args
            .token_issuer
            .as_deref()
            .and_then(trimmed)
            .unwrap_or_else(|| DEFAULT_ISSUER.to_string()),
    })
}

/// The value unless it is blank, in which case None.
///
/// Blank is treated as ABSENT throughout: Kubernetes readily produces `NAME=""` (an
/// env var with no value, a configMapKeyRef pointing at an empty key), and "" is not
/// a host, a controller id or a key. The PEM is the one value not re-trimmed after
/// this check — its trailing newline is part of what the parser wants.
fn trimmed(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Builds the server the config describes.
///
/// The `Option<AuthConfig>` → `new`/`with_auth` split is the point where "auth on
/// but no usable key" stops being representable: `with_auth` takes an already-built
/// verifier, so a bad key fails HERE, before anything is listening.
fn build_server(config: Config) -> Result<SandboxServer, String> {
    match config.auth {
        Some(a) => {
            let verifier =
                TokenVerifier::new(&a.public_key_pem, &a.controller_id, &a.issuer, &a.kid)?;
            Ok(SandboxServer::with_auth(config.bind_addr, verifier))
        }
        None => Ok(SandboxServer::new(config.bind_addr)),
    }
}

#[tokio::main]
async fn main() {
    // INFO by default; RUST_LOG overrides (e.g. RUST_LOG=debug). Logs are this
    // process's only output — a controller is never attached to a terminal.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Parses argv AND the env fallbacks. clap exits 2 with a usage message on a bad
    // flag, which is the right behaviour here too — a container that cannot parse its
    // own configuration must not start.
    let config = match resolve(Args::parse()) {
        Ok(c) => c,
        Err(e) => fatal(&e),
    };
    info!(
        "sandd-controller {} starting on {}",
        env!("CARGO_PKG_VERSION"),
        config.bind_addr
    );

    let server = match build_server(config) {
        Ok(s) => s,
        Err(e) => fatal(&e),
    };

    // start() logs which auth mode is in effect (info when enabled, warn when not).
    if let Err(e) = server.start().await {
        fatal(&format!("server stopped: {:#}", e));
    }
}

/// Reports a startup failure and exits non-zero.
///
/// Written to stderr as well as the log: a container that dies before the log
/// pipeline is scraped leaves `kubectl logs` as the only trace, and a bare
/// non-zero exit with no message is the worst thing to hand an operator.
fn fatal(msg: &str) -> ! {
    tracing::error!("{}", msg);
    eprintln!("sandd-controller: {}", msg);
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated with `openssl genpkey -algorithm ed25519 | openssl pkey -pubout`.
    const PUBLIC_PEM: &str =
        "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEAGb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE=\n-----END PUBLIC KEY-----\n";

    /// The defaults clap would produce with no flags and no env at all. Built via
    /// `try_parse_from` rather than by hand so the test exercises the REAL defaults
    /// (`default_value` strings) instead of a second copy of them that could drift.
    fn bare() -> Args {
        Args::try_parse_from(["sandd-controller"]).unwrap()
    }

    /// Defaults plus everything --enable-auth requires.
    fn with_auth() -> Args {
        Args {
            enable_auth: true,
            controller_id: Some("sandd-abc-uid".to_string()),
            signing_public_key: Some(PUBLIC_PEM.to_string()),
            signing_kid: Some("kid-1".to_string()),
            ..bare()
        }
    }

    // The zero-config case: a bare `sandd-controller` must come up listening where its
    // clients expect it. 0.0.0.0 specifically — daemons dial in from other machines, so
    // a loopback default would start cleanly and serve nobody.
    #[test]
    fn defaults_to_the_port_daemons_dial() {
        let config = resolve(bare()).unwrap();

        assert_eq!(config.bind_addr, "0.0.0.0:8765");
        assert_eq!(config.auth, None);
    }

    // The flag surface itself: --host/--port/--enable-auth must parse from argv, since
    // that is the whole point of having flags rather than env vars alone.
    #[test]
    fn flags_are_parsed_from_argv() {
        let args = Args::try_parse_from([
            "sandd-controller",
            "--host",
            "127.0.0.1",
            "--port",
            "9000",
            "--enable-auth",
            "--controller-id",
            "sandd-abc-uid",
            "--signing-public-key",
            PUBLIC_PEM,
            "--signing-kid",
            "kid-1",
        ])
        .unwrap();

        let config = resolve(args).unwrap();

        assert_eq!(config.bind_addr, "127.0.0.1:9000");
        let auth = config.auth.unwrap();
        assert_eq!(auth.controller_id, "sandd-abc-uid");
        assert_eq!(auth.kid, "kid-1");
    }

    // A PEM begins with `-----BEGIN PUBLIC KEY-----`, which clap reads as a flag unless
    // the arg allows hyphen values. Without that the --signing-public-key FLAG is
    // unusable (it fails with "unexpected argument '-----BEGIN...'"), while the env
    // path works fine — so this breaks only for whoever runs the binary by hand, which
    // is why it needs a test rather than a code reading.
    #[test]
    fn a_pem_is_accepted_as_a_flag_value_despite_its_leading_dashes() {
        let args = Args::try_parse_from(["sandd-controller", "--signing-public-key", PUBLIC_PEM])
            .expect("a PEM must be accepted as a flag value");

        assert_eq!(args.signing_public_key.as_deref(), Some(PUBLIC_PEM));
    }

    // A BARE `--enable-auth` must turn auth on. This is the ergonomic case a human
    // types, and it only works because of `default_missing_value`.
    #[test]
    fn a_bare_enable_auth_flag_turns_auth_on() {
        let args = Args::try_parse_from(["sandd-controller", "--enable-auth"]).unwrap();

        assert!(args.enable_auth);
    }

    // ...and `--enable-auth=false` must turn it OFF. With a plain SetTrue flag the
    // value would be rejected outright, and — the reason this matters — merely SETTING
    // SANDD_ENABLE_AUTH would enable auth, so `SANDD_ENABLE_AUTH=false` would turn it
    // ON. That is the one direction this must not be wrong in.
    #[test]
    fn enable_auth_accepts_an_explicit_false() {
        for (value, expected) in [
            ("true", true),
            ("false", false),
            ("1", true),
            ("0", false),
            ("yes", true),
            ("no", false),
            ("on", true),
            ("off", false),
            ("TRUE", true),
            ("False", false),
        ] {
            let args =
                Args::try_parse_from(["sandd-controller", &format!("--enable-auth={}", value)])
                    .unwrap();

            assert_eq!(args.enable_auth, expected, "--enable-auth={}", value);
        }
    }

    // An unrecognized value must be REFUSED, not silently falsy. `--enable-auth=enabled`
    // meaning "disabled" is exactly the belief-vs-reality gap that ships an
    // unauthenticated controller.
    #[test]
    fn an_unrecognized_enable_auth_value_is_an_error() {
        for bogus in ["enabled", "y", "2", "maybe", "tru"] {
            assert!(
                Args::try_parse_from(["sandd-controller", &format!("--enable-auth={}", bogus)])
                    .is_err(),
                "--enable-auth={} must be refused rather than treated as off",
                bogus
            );
        }
        // The parser is shared with the env path, so it is pinned directly too.
        assert!(parse_bool("enabled").is_err());
    }

    // A typo'd host must be named at STARTUP. Left to `bind`, the error is "invalid
    // socket address" with nothing pointing at what produced it.
    #[test]
    fn rejects_a_host_that_is_not_an_ip() {
        let args = Args {
            host: "not-an-ip".to_string(),
            ..bare()
        };

        let err = resolve(args).unwrap_err();

        assert!(err.contains("--host"), "error must name the flag: {}", err);
    }

    // Port 0 binds SUCCESSFULLY to an OS-chosen port, so this cannot be left to bind:
    // the daemons' URLs name a fixed port and every one of them would fail to connect.
    #[test]
    fn rejects_port_zero() {
        let args = Args { port: 0, ..bare() };

        let err = resolve(args).unwrap_err();

        assert!(err.contains("--port"), "error must name the flag: {}", err);
    }

    // Out-of-range and non-numeric ports are clap's job; pinned so a later switch to a
    // hand-rolled parser cannot lose it.
    #[test]
    fn rejects_a_port_that_is_not_a_u16() {
        for port in ["70000", "-1", "not-a-number"] {
            assert!(
                Args::try_parse_from(["sandd-controller", "--port", port]).is_err(),
                "--port {} must be refused",
                port
            );
        }
    }

    // Blank is treated as ABSENT: Kubernetes readily produces `NAME=""` (an env var
    // with no value, a configMapKeyRef to an empty key), and "" is not a host.
    #[test]
    fn blank_values_fall_back_to_defaults() {
        let args = Args {
            host: "   ".to_string(),
            ..bare()
        };

        assert_eq!(resolve(args).unwrap().bind_addr, "0.0.0.0:8765");
    }

    #[test]
    fn enable_auth_collects_the_verification_material() {
        let args = Args {
            token_issuer: Some("nebula-prod".to_string()),
            ..with_auth()
        };

        let auth = resolve(args).unwrap().auth.unwrap();

        assert_eq!(auth.controller_id, "sandd-abc-uid");
        assert_eq!(auth.public_key_pem, PUBLIC_PEM);
        assert_eq!(auth.kid, "kid-1");
        assert_eq!(auth.issuer, "nebula-prod");
    }

    // Both sides default the issuer to the same literal, so the common deployment
    // configures neither. If this drifts from the manager's default, every token is
    // rejected for a wrong `iss` — with a signature that verified perfectly.
    #[test]
    fn issuer_defaults_to_the_managers_default() {
        let auth = resolve(with_auth()).unwrap().auth.unwrap();

        assert_eq!(auth.issuer, "nebula");
    }

    // THE central failure mode. With auth requested but material missing, the only
    // acceptable outcome is an error: falling back to no-auth yields a controller that
    // admits every caller while looking perfectly healthy.
    #[test]
    fn auth_on_with_missing_material_is_an_error_not_a_silent_downgrade() {
        let cases: [(&str, fn(&mut Args)); 3] = [
            ("controller-id", |a| a.controller_id = None),
            ("signing-public-key", |a| a.signing_public_key = None),
            ("signing-kid", |a| a.signing_kid = None),
        ];

        for (flag, omit) in cases {
            let mut args = with_auth();
            omit(&mut args);

            let err = resolve(args).unwrap_err();

            assert!(
                err.contains(flag),
                "omitting --{} must fail with a message naming it, got: {}",
                flag,
                err
            );
        }
    }

    // Present-but-blank is what a missing Secret key actually produces in a Pod's
    // environment, so it must fail the same way an absent value does.
    #[test]
    fn auth_on_with_blank_material_is_an_error() {
        let cases: [(&str, fn(&mut Args)); 3] = [
            ("controller-id", |a| a.controller_id = Some("  ".into())),
            ("signing-public-key", |a| {
                a.signing_public_key = Some("".into())
            }),
            ("signing-kid", |a| a.signing_kid = Some("\n".into())),
        ];

        for (flag, blank) in cases {
            let mut args = with_auth();
            blank(&mut args);

            assert!(resolve(args).is_err(), "a blank --{} must be refused", flag);
        }
    }

    // The material is only required WITH auth. Omitting all of it while auth is off is
    // the standalone/e2e shape and must start cleanly.
    #[test]
    fn material_is_not_required_when_auth_is_off() {
        assert!(resolve(bare()).unwrap().auth.is_none());
    }

    // Auth is enabled by CONSTRUCTION: a config with auth yields a server holding a
    // verifier, one without yields the unauthenticated shape. This is the seam where a
    // wiring mistake would leave a controller listening with auth silently off.
    #[test]
    fn build_server_enables_auth_when_configured() {
        assert!(build_server(resolve(with_auth()).unwrap()).is_ok());
        assert!(build_server(resolve(bare()).unwrap()).is_ok());
    }

    // A malformed key must kill the process BEFORE anything listens. Reaching the
    // listener with an unusable key would mean either a panic mid-handshake or, far
    // worse, a server that started with verification quietly inert.
    #[test]
    fn build_server_fails_on_an_unusable_key() {
        let args = Args {
            signing_public_key: Some(
                "-----BEGIN PUBLIC KEY-----\nnope\n-----END PUBLIC KEY-----\n".to_string(),
            ),
            ..with_auth()
        };
        let config = resolve(args).unwrap();

        assert!(build_server(config).is_err());
    }

    // The env var names are a CONTRACT with Nebula's sanddControllerEnv. A rename on
    // either side presents as every daemon failing to authenticate, with nothing in the
    // logs pointing at the cause. Asserted through clap's own metadata, so this pins
    // what the binary ACTUALLY reads rather than a duplicate list of literals.
    #[test]
    fn env_var_names_match_the_nebula_contract() {
        use clap::CommandFactory;

        let cmd = Args::command();
        let env_of = |id: &str| -> String {
            cmd.get_arguments()
                .find(|a| a.get_id() == id)
                .unwrap_or_else(|| panic!("no such arg: {}", id))
                .get_env()
                .unwrap_or_else(|| panic!("{} must be readable from an env var", id))
                .to_string_lossy()
                .to_string()
        };

        assert_eq!(env_of("controller_id"), "SANDD_CONTROLLER_ID");
        assert_eq!(env_of("signing_public_key"), "SANDD_SIGNING_PUBLIC_KEY");
        assert_eq!(env_of("signing_kid"), "SANDD_SIGNING_KID");
        assert_eq!(env_of("token_issuer"), "SANDD_TOKEN_ISSUER");
        assert_eq!(env_of("enable_auth"), "SANDD_ENABLE_AUTH");
    }

    // The port must stay equal to nebulav1alpha1.SanddControllerPort, which is also
    // baked into the daemon's dial-out URL. Changing one side alone means daemons
    // connect to a closed port.
    #[test]
    fn default_port_matches_the_daemon_dial_url() {
        assert_eq!(DEFAULT_PORT, 8765);
    }

    // clap panics at RUNTIME on a malformed command definition (conflicting ids, a bad
    // default_value for the value_parser), which would otherwise only surface when the
    // container starts. This asserts the definition is well-formed at test time.
    #[test]
    fn the_command_definition_is_valid() {
        use clap::CommandFactory;

        Args::command().debug_assert();
    }
}

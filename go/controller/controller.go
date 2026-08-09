/*
Copyright 2026 The InftyAI Team.

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
*/

// Package controller runs the SandD controller inside a Go process.
//
// The controller — the WebSocket server daemons dial into, the registry that holds their
// sockets, and the token verification that admits them — is this repo's Rust
// implementation, linked in as a static archive and driven through its C ABI
// (server/src/ffi.rs). Nothing here reimplements the protocol: this package is a safe Go
// skin over pointers, the same way python/ is a safe Python skin over the same registry.
//
// It lives in THIS repo, beside the code it wraps, so an ABI change and its binding move
// in one commit. A copy maintained in a consumer would drift silently — a linker catches
// a missing symbol, never a changed meaning.
//
// # Why a host embeds this instead of calling a controller over the network
//
// A daemon's connection is a live socket in whichever process called accept(), and it
// cannot be observed or handed to another process. A host can therefore reach a daemon
// only by holding the socket itself or by asking the holder over the network. Embedding
// removes that second process — for Nebula, a Deployment, the public/private signing-key
// split, and the kid/iss/aud agreement between two processes that has historically been
// the most common misconfiguration.
//
// WHAT EMBEDDING COSTS, so it is not discovered later:
//   - Shared crash domain. A panic in the Rust half takes the host process down with it.
//   - Whatever public route reaches the controller now terminates on the host, which for
//     Nebula is the pod holding the private signing key.
//   - The host's memory limit must cover every daemon socket (see Config.Bind).
//
// # Linking
//
// The archive must be built with the ffi feature:
//
//	cargo build -p sandbox-server --features ffi --lib --release
//
// and found at link time, e.g. CGO_LDFLAGS=-L<repo>/target/release. Built for a musl
// target it links FULLY STATIC, so a cgo host keeps a self-contained binary and can still
// ship on a distroless/static base.
//
// # Concurrency and lifetime
//
// The C ABI cannot make use-after-free unrepresentable, so this package does: handles
// live behind a mutex-guarded pointer that is nil'd on Close, every entry point checks
// it, and a closed handle returns ErrClosed rather than dereferencing freed memory. That
// is the whole reason this file exists instead of callers using cgo directly.
//
// Nil'ing the pointer is NOT sufficient on its own. Exec and Session.Read park in C for
// up to their timeout and must not hold the mutex while they do — otherwise one idle
// terminal serializes every other caller — so they copy the handle and release the lock.
// A concurrent Close would then free a handle a blocked call is still using, and on the
// server that means dropping the tokio runtime the call is executing on. Both types
// therefore count in-flight calls and Close WAITS for that count to drain before
// freeing. Consequence for callers: Close can block for as long as the longest
// outstanding timeout, and must not be called while holding a lock a reader needs.
//
// A Server must outlive every Session opened from it — a Session borrows the server's
// tokio runtime handle. Server.Close closes all sessions, and waits for each, for
// exactly that reason.
package controller

/*
#cgo LDFLAGS: -lsandbox_server -lm
#include <stdlib.h>
#include <stdint.h>

typedef struct SanddServer SanddServer;
typedef struct SanddSession SanddSession;

SanddServer* sandd_server_start(const char* bind_addr, const char* public_key_pem,
                                const char* controller_id, const char* issuer,
                                const char* kid);
void  sandd_server_free(SanddServer*);
int   sandd_server_daemon_count(const SanddServer*);
char* sandd_server_stats_json(const SanddServer*);

int   sandd_exec(const SanddServer*, const char* daemon_id, const char* command,
                 uint64_t timeout_secs, char** out_json);

SanddSession* sandd_session_open(const SanddServer*, const char* daemon_id,
                                 uint16_t rows, uint16_t cols, const char* term);
int   sandd_session_write(const SanddSession*, const uint8_t* data, size_t len);
int   sandd_session_read(const SanddSession*, uint8_t* out, size_t cap,
                         uint64_t timeout_ms);
int   sandd_session_resize(const SanddSession*, uint16_t rows, uint16_t cols);
void  sandd_session_free(SanddSession*);
char* sandd_session_id(const SanddSession*);

const char* sandd_last_error(void);
void  sandd_string_free(char*);
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"time"
	"unsafe"
)

// Return codes from the C ABI. Mirrors the SANDD_* constants in ffi.rs; a change
// there without a change here is a silent misclassification, so they are asserted
// against the header's documented values in the tests.
const (
	rcOK       = 0
	rcErr      = -1
	rcNoDaemon = -2
	rcTimeout  = -3
	rcClosed   = -4
)

// ReadBufSize is the smallest buffer Session.Read may be given: the underlying channel
// yields whole chunks, so a shorter buffer DISCARDS the tail of one rather than resuming
// it on the next call (SANDD_READ_BUF_MIN in ffi.rs).
//
// EXPORTED because it is a correctness floor, not a tuning knob, and the caller allocates
// the buffer. A host that hard-codes 64KiB of its own looks correct and starts silently
// truncating output the day this floor rises — the exact drift this binding exists to
// prevent, so the number must be referenced, never copied.
const ReadBufSize = 64 * 1024

var (
	// ErrClosed is returned by every method on a Server or Session that has been
	// closed. It exists so a late caller gets an error instead of dereferencing freed
	// memory, which is the failure mode this package is built to prevent.
	ErrClosed = errors.New("sandd: handle is closed")

	// ErrNoDaemon means the daemon id is not in the registry: it never connected, or it
	// was reaped or disconnected. Distinct from a generic failure so callers can map it
	// to a NotFound rather than an internal error.
	ErrNoDaemon = errors.New("sandd: daemon not connected")

	// ErrSessionClosed means the session ended — the daemon exited, disconnected, or
	// closed the PTY. Terminal: no further reads will succeed.
	ErrSessionClosed = errors.New("sandd: session closed")
)

// Config configures the embedded controller.
type Config struct {
	// Bind is the listen address for daemon dial-ins, e.g. "0.0.0.0:8765".
	//
	// Every connected daemon costs this process roughly 15-50KB (tokio task, framing
	// buffers, registry entry, kernel socket buffers), so the manager's memory limit
	// must cover the expected fleet. Exceeding it presents as an OOMKill, which reads
	// like a crash loop rather than like capacity — the reason that number is stated
	// here rather than left to be discovered.
	Bind string

	// PublicKeyPEM and ControllerID enable authentication and MUST be set together.
	// Empty means auth is DISABLED, which admits any caller that speaks the protocol
	// and is for tests only. Half-configured is rejected by the Rust side rather than
	// silently downgraded.
	//
	// ControllerID is the ONLY audience admitted, and must equal the aud the manager
	// mints (see pkg/sandd.Signer).
	PublicKeyPEM string
	ControllerID string

	// Issuer and KID must match what the minter puts in the token. Issuer defaults to
	// "nebula"; an empty KID accepts any key id, which is what lets a rotation present
	// old and new keys.
	Issuer string
	KID    string
}

// Server is the embedded SandD controller.
type Server struct {
	// mu guards ptr and sessions. Held only around pointer bookkeeping, never across a
	// blocking C call: sandd_exec parks for up to its timeout, and holding mu there
	// would serialize every exec in the process behind one slow command.
	mu       sync.Mutex
	ptr      *C.SanddServer
	sessions map[*Session]struct{}

	// inflight counts calls that have copied ptr and are executing in C right now.
	//
	// Required because Exec releases mu for the duration of its call: nil'ing ptr is
	// then NOT enough to make freeing safe, since a blocked call still holds its own
	// copy. sandd_server_free drops the tokio Runtime that sandd_exec is parked on, so
	// freeing underneath one is a use-after-free of a running executor, not merely a
	// stale handle. Close waits for this to drain before freeing.
	//
	// Add is only ever called under mu with ptr non-nil, and Close nils ptr under mu
	// before it Waits — so no Add can race a Wait.
	inflight sync.WaitGroup
}

// acquire hands out the raw handle and registers an in-flight C call, or fails if the
// server is closed. Every acquire MUST be paired with a release, hence the defer at each
// call site: a leaked count wedges Close forever.
func (s *Server) acquire() (*C.SanddServer, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ptr == nil {
		return nil, ErrClosed
	}
	s.inflight.Add(1)
	return s.ptr, nil
}

func (s *Server) release() { s.inflight.Done() }

// Start launches the controller. The returned Server must be Closed to release the
// listening socket and every daemon connection.
func Start(cfg Config) (*Server, error) {
	if cfg.Bind == "" {
		return nil, errors.New("sandd: Config.Bind is required")
	}
	// Caught here rather than at the boundary so the error names the Go field.
	if (cfg.PublicKeyPEM == "") != (cfg.ControllerID == "") {
		return nil, errors.New(
			"sandd: PublicKeyPEM and ControllerID must be set together " +
				"(both empty disables authentication)")
	}

	bind := C.CString(cfg.Bind)
	defer C.free(unsafe.Pointer(bind))

	// NULL, not "", for the auth material: the C side treats both-NULL as "auth off"
	// and an empty string as a misconfiguration, so an empty CString would be rejected
	// rather than disabling auth.
	var key, id, iss, kid *C.char
	if cfg.PublicKeyPEM != "" {
		key = C.CString(cfg.PublicKeyPEM)
		defer C.free(unsafe.Pointer(key))
		id = C.CString(cfg.ControllerID)
		defer C.free(unsafe.Pointer(id))

		issuer := cfg.Issuer
		if issuer == "" {
			issuer = "nebula"
		}
		iss = C.CString(issuer)
		defer C.free(unsafe.Pointer(iss))
		kid = C.CString(cfg.KID)
		defer C.free(unsafe.Pointer(kid))
	}

	ptr := C.sandd_server_start(bind, key, id, iss, kid)
	if ptr == nil {
		return nil, fmt.Errorf("sandd: failed to start controller: %s", lastError())
	}
	return &Server{ptr: ptr, sessions: make(map[*Session]struct{})}, nil
}

// Close stops the controller, dropping every daemon socket it holds.
//
// Sessions are closed FIRST and their handles freed before the server's: a Session
// borrows the server's tokio runtime, so freeing the server while one is open would
// leave a dangling handle. Close is idempotent.
//
// BLOCKS until every in-flight call returns. ptr is nil'd first, so callers arriving
// after this point get ErrClosed and cannot join the set being waited on; the ones
// already parked in C are waited out because sandd_server_free drops the runtime they
// are running on. An Exec with a long timeout therefore delays Close by up to that
// timeout — bounded, and the alternative is freeing a live executor.
func (s *Server) Close() error {
	s.mu.Lock()
	if s.ptr == nil {
		s.mu.Unlock()
		return nil
	}
	open := make([]*Session, 0, len(s.sessions))
	for sess := range s.sessions {
		open = append(open, sess)
	}
	ptr := s.ptr
	s.ptr = nil
	s.sessions = nil
	s.mu.Unlock()

	// Outside s.mu: Session.Close calls back into s.forget, which takes it. Each of
	// these waits out its own parked reader, so sessions are fully quiescent before the
	// server's runtime goes away.
	for _, sess := range open {
		_ = sess.Close()
	}

	// After the sessions: a session's read parks on a handle to THIS runtime, so
	// draining them first is what makes waiting here sufficient.
	s.inflight.Wait()

	C.sandd_server_free(ptr)
	return nil
}

// DaemonCount reports how many daemons are currently connected.
func (s *Server) DaemonCount() (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ptr == nil {
		return 0, ErrClosed
	}
	n := C.sandd_server_daemon_count(s.ptr)
	if n < 0 {
		return 0, fmt.Errorf("sandd: %s", lastError())
	}
	return int(n), nil
}

// DaemonInfo describes one connected daemon.
type DaemonInfo struct {
	Hostname string            `json:"hostname"`
	Platform string            `json:"platform"`
	Arch     string            `json:"arch"`
	Version  string            `json:"version"`
	Labels   map[string]string `json:"labels"`
	IsBusy   bool              `json:"is_busy"`
	// ConnectedSecs is how long this daemon has been connected.
	ConnectedSecs uint64 `json:"connected_secs"`
	// SecondsSinceHeartbeat, compared against the controller's reap threshold, says how
	// close this daemon is to being dropped from the registry.
	SecondsSinceHeartbeat uint64 `json:"seconds_since_heartbeat"`
}

// Stats is the registry snapshot, keyed by daemon id.
type Stats struct {
	TotalDaemons         int                   `json:"total_daemons"`
	ByPlatform           map[string]int        `json:"by_platform"`
	OldestConnectionSecs uint64                `json:"oldest_connection_secs"`
	Daemons              map[string]DaemonInfo `json:"daemons"`
}

// Stats returns a snapshot of the registry.
func (s *Server) Stats() (*Stats, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ptr == nil {
		return nil, ErrClosed
	}
	raw := C.sandd_server_stats_json(s.ptr)
	if raw == nil {
		return nil, fmt.Errorf("sandd: %s", lastError())
	}
	defer C.sandd_string_free(raw)

	var out Stats
	if err := json.Unmarshal([]byte(C.GoString(raw)), &out); err != nil {
		return nil, fmt.Errorf("sandd: decode stats: %w", err)
	}
	return &out, nil
}

// ExecResult is the outcome of a one-shot command.
type ExecResult struct {
	Stdout     string `json:"stdout"`
	Stderr     string `json:"stderr"`
	ExitCode   int    `json:"exit_code"`
	DurationMS uint64 `json:"duration_ms"`
}

// Exec runs command on a daemon and blocks until it completes or timeout elapses.
//
// One-shot: the result is complete stdout/stderr after the fact. That backs
// `kubectl exec -- ls` but not an interactive shell; use OpenSession for a PTY.
//
// A timeout is NOT retryable. The command may have run — a timeout says only that no
// answer arrived — so retrying risks executing it twice.
func (s *Server) Exec(daemonID, command string, timeout time.Duration) (*ExecResult, error) {
	// acquire, not a bare pointer copy: this call outlives its hold on s.mu, so it must
	// keep Close from freeing the handle underneath it.
	ptr, err := s.acquire()
	if err != nil {
		return nil, err
	}
	defer s.release()

	cid := C.CString(daemonID)
	defer C.free(unsafe.Pointer(cid))
	ccmd := C.CString(command)
	defer C.free(unsafe.Pointer(ccmd))

	secs := uint64(timeout.Seconds())
	if secs == 0 {
		secs = 1 // A zero timeout means "no time at all" to the C side, never "no limit".
	}

	var out *C.char
	// Blocks for up to `timeout` without holding s.mu, so concurrent execs to different
	// daemons proceed in parallel.
	rc := C.sandd_exec(ptr, cid, ccmd, C.uint64_t(secs), &out)
	if rc != rcOK {
		if rc == rcNoDaemon {
			return nil, fmt.Errorf("%w: %s", ErrNoDaemon, daemonID)
		}
		return nil, fmt.Errorf("sandd: exec on %s: %s", daemonID, lastError())
	}
	if out == nil {
		return nil, errors.New("sandd: exec returned no result")
	}
	defer C.sandd_string_free(out)

	var res ExecResult
	if err := json.Unmarshal([]byte(C.GoString(out)), &res); err != nil {
		return nil, fmt.Errorf("sandd: decode exec result: %w", err)
	}
	return &res, nil
}

// forget drops a session from the server's tracking set. Called by Session.Close.
func (s *Server) forget(sess *Session) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.sessions != nil {
		delete(s.sessions, sess)
	}
}

// Session is one interactive PTY on a daemon. Safe for one reader and one writer
// concurrently, which is what a terminal relay needs.
type Session struct {
	mu  sync.Mutex
	ptr *C.SanddSession
	srv *Server
	id  string
	buf []byte // Reused across Reads; guarded by readMu, not mu.

	// readMu serializes Read so two concurrent readers cannot share buf and interleave
	// output. Separate from mu because Read must not hold mu while parked in C.
	readMu sync.Mutex

	// inflight counts reads parked in C, for the same reason as Server.inflight: Read
	// releases mu before blocking, so nil'ing ptr does not stop a parked call from
	// holding its own copy. Close waits for it before freeing the handle.
	inflight sync.WaitGroup

	// free releases the handle. nil means sandd_session_free — overridden only by
	// stubSession, so a test can observe WHEN the free happens.
	free func(*C.SanddSession)
}

// acquire hands out the raw handle and registers an in-flight C call, or fails if the
// session is closed. Must be paired with a release.
func (s *Session) acquire() (*C.SanddSession, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ptr == nil {
		return nil, ErrClosed
	}
	s.inflight.Add(1)
	return s.ptr, nil
}

func (s *Session) release() { s.inflight.Done() }

// stubSession builds a Session whose handle is non-nil but never reaches the C free, so a
// test can assert the ORDER of "wait for parked reads, then free".
//
// It lives in this file, not the test, for two reasons: a real Session needs a connected
// daemon, which this dependency-free module cannot fake, and cgo is not permitted in
// _test.go files at all — so anything naming *C.SanddSession has to be here. The handle is
// a 1-byte malloc that is never dereferenced, freed by the stub itself.
func stubSession(onFree func()) *Session {
	return &Session{
		ptr: (*C.SanddSession)(C.malloc(1)),
		buf: make([]byte, ReadBufSize),
		free: func(ptr *C.SanddSession) {
			C.free(unsafe.Pointer(ptr))
			onFree()
		},
	}
}

// OpenSession starts an interactive session on a daemon with the given terminal
// geometry. term may be empty for "xterm-256color".
func (s *Server) OpenSession(daemonID string, rows, cols uint16, term string) (*Session, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ptr == nil {
		return nil, ErrClosed
	}

	cid := C.CString(daemonID)
	defer C.free(unsafe.Pointer(cid))

	var cterm *C.char
	if term != "" {
		cterm = C.CString(term)
		defer C.free(unsafe.Pointer(cterm))
	}

	ptr := C.sandd_session_open(s.ptr, cid, C.uint16_t(rows), C.uint16_t(cols), cterm)
	if ptr == nil {
		// The C side does not distinguish "no such daemon" from other open failures by
		// return value (it returns NULL either way), so the message is the only signal.
		return nil, fmt.Errorf("sandd: open session on %s: %s", daemonID, lastError())
	}

	sess := &Session{ptr: ptr, srv: s, buf: make([]byte, ReadBufSize)}
	if raw := C.sandd_session_id(ptr); raw != nil {
		sess.id = C.GoString(raw)
		C.sandd_string_free(raw)
	}
	s.sessions[sess] = struct{}{}
	return sess, nil
}

// ID is the controller-assigned session id, for correlating logs across the two halves.
func (s *Session) ID() string { return s.id }

// Write sends stdin to the session.
func (s *Session) Write(p []byte) (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ptr == nil {
		return 0, ErrClosed
	}
	if len(p) == 0 {
		return 0, nil
	}

	rc := C.sandd_session_write(s.ptr, (*C.uint8_t)(&p[0]), C.size_t(len(p)))
	// p is borrowed by the C call only for its duration — the Rust side copies into a
	// protocol message before returning — but the Go GC must not move or collect it
	// mid-call, which this guarantees.
	runtime.KeepAlive(p)

	if rc != rcOK {
		if rc == rcNoDaemon {
			return 0, ErrNoDaemon
		}
		return 0, fmt.Errorf("sandd: session write: %s", lastError())
	}
	return len(p), nil
}

// Read copies session output into p, blocking at most timeout.
//
// Returns (0, nil) when the timeout elapses with no output. That is the NORMAL state of
// an idle terminal, not an error, so callers loop on it; only ErrSessionClosed is
// terminal. Reporting idleness as an error would make every quiet shell look broken.
//
// p should be at least ReadBufSize: the underlying channel yields whole chunks, and a
// short buffer discards the tail of one rather than resuming it on the next call.
func (s *Session) Read(p []byte, timeout time.Duration) (int, error) {
	s.readMu.Lock()
	defer s.readMu.Unlock()

	// acquire, not a bare pointer copy: this parks in C without holding s.mu, so it must
	// keep Close from freeing the handle underneath it.
	ptr, err := s.acquire()
	if err != nil {
		return 0, err
	}
	defer s.release()

	if len(p) == 0 {
		return 0, nil
	}

	ms := uint64(timeout.Milliseconds())
	if ms == 0 {
		ms = 1
	}

	// Parks in C for up to `timeout` WITHOUT holding s.mu, so a concurrent Write or
	// Resize is not blocked behind an idle read.
	rc := C.sandd_session_read(ptr, (*C.uint8_t)(&p[0]), C.size_t(len(p)), C.uint64_t(ms))
	runtime.KeepAlive(p)

	switch {
	case rc >= 0:
		return int(rc), nil
	case rc == rcTimeout:
		return 0, nil
	case rc == rcClosed:
		return 0, ErrSessionClosed
	default:
		return 0, fmt.Errorf("sandd: session read: %s", lastError())
	}
}

// Resize tells the daemon the terminal geometry changed.
func (s *Session) Resize(rows, cols uint16) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ptr == nil {
		return ErrClosed
	}
	rc := C.sandd_session_resize(s.ptr, C.uint16_t(rows), C.uint16_t(cols))
	if rc != rcOK {
		if rc == rcNoDaemon {
			return ErrNoDaemon
		}
		return fmt.Errorf("sandd: session resize: %s", lastError())
	}
	return nil
}

// Close ends the session and frees its handle. Idempotent.
//
// BLOCKS until a reader parked in Read returns, which takes up to that read's timeout.
// The parked call holds its own pointer copy, so nil'ing ptr does not protect it —
// waiting is what makes the free safe. Such a reader sees whatever its own call returned
// (ErrSessionClosed once the daemon drops the channel, or a timeout), and only a reader
// arriving AFTER this gets ErrClosed.
//
// Callers must therefore not hold a lock that a reader needs, and a Read timeout doubles
// as the worst-case Close latency — keep it short (Nebula's relay polls at 500ms and
// loops, rather than parking for the session's whole lifetime) rather than unbounded.
func (s *Session) Close() error {
	s.mu.Lock()
	if s.ptr == nil {
		s.mu.Unlock()
		return nil
	}
	ptr := s.ptr
	s.ptr = nil
	s.mu.Unlock()

	// Before the free, and outside s.mu so a parked read can finish: it is holding a copy
	// of ptr and running on the server's runtime.
	s.inflight.Wait()

	if s.srv != nil {
		s.srv.forget(s)
	}
	if s.free != nil {
		s.free(ptr)
	} else {
		C.sandd_session_free(ptr)
	}
	return nil
}

// lastError reads the calling thread's error message from the C side.
//
// MUST be called on the same OS thread that saw the failure — the message is
// thread-local in Rust. cgo pins the calling goroutine to its OS thread for the duration
// of a C call, so a lastError() invoked immediately after a failed call in the same Go
// statement sequence is on the right thread. Any goroutine switch in between and the
// message is lost, so every call site here fetches it directly after the failure with no
// intervening operation.
func lastError() string {
	msg := C.sandd_last_error()
	if msg == nil {
		return "unknown error"
	}
	return C.GoString(msg)
}

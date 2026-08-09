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

package controller

import (
	"errors"
	"strings"
	"sync"
	"testing"
	"time"
)

// Every test binds an EXPLICIT distinct port on loopback. The controller's listener is
// spawned and not awaited, so a bind collision surfaces as "no daemon ever connects"
// rather than as a Start error — sharing a port between tests would produce a passing
// test that measured nothing.
const (
	portMisconfig = "127.0.0.1:19101"
	portLifecycle = "127.0.0.1:19102"
	portNoDaemon  = "127.0.0.1:19103"
	portErrs      = "127.0.0.1:19104"
	portInflight  = "127.0.0.1:19105"
	portCloseRace = "127.0.0.1:19106"
)

func TestStartRejectsHalfConfiguredAuth(t *testing.T) {
	// The dangerous direction: a caller that MEANT to enable auth but supplied only one
	// of the two fields must not get a controller that admits everyone.
	cases := []struct {
		name string
		cfg  Config
	}{
		{"key without controller id", Config{Bind: portMisconfig, PublicKeyPEM: "pem"}},
		{"controller id without key", Config{Bind: portMisconfig, ControllerID: "sandd-abc"}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			srv, err := Start(tc.cfg)
			if err == nil {
				srv.Close()
				t.Fatal("expected an error, got a running controller with auth disabled")
			}
			if !strings.Contains(err.Error(), "must be set together") {
				t.Errorf("error should explain the pairing requirement, got: %v", err)
			}
		})
	}
}

func TestStartRequiresBind(t *testing.T) {
	if _, err := Start(Config{}); err == nil {
		t.Fatal("expected an error for an empty Bind")
	}
}

func TestServerLifecycle(t *testing.T) {
	srv, err := Start(Config{Bind: portLifecycle})
	if err != nil {
		t.Fatalf("Start: %v", err)
	}

	n, err := srv.DaemonCount()
	if err != nil {
		t.Fatalf("DaemonCount: %v", err)
	}
	if n != 0 {
		t.Errorf("DaemonCount = %d, want 0 with no daemon connected", n)
	}

	stats, err := srv.Stats()
	if err != nil {
		t.Fatalf("Stats: %v", err)
	}
	if stats.TotalDaemons != 0 {
		t.Errorf("TotalDaemons = %d, want 0", stats.TotalDaemons)
	}
	// Decoding must produce usable maps, not nil, or callers range over nil silently.
	if stats.Daemons == nil || stats.ByPlatform == nil {
		t.Error("Stats maps should be non-nil after decoding")
	}

	if err := srv.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	// Idempotent: a double Close must not double-free.
	if err := srv.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}

	// The point of the wrapper: post-Close calls error instead of dereferencing freed
	// memory.
	if _, err := srv.DaemonCount(); !errors.Is(err, ErrClosed) {
		t.Errorf("DaemonCount after Close = %v, want ErrClosed", err)
	}
	if _, err := srv.Stats(); !errors.Is(err, ErrClosed) {
		t.Errorf("Stats after Close = %v, want ErrClosed", err)
	}
	if _, err := srv.Exec("d-1", "true", time.Second); !errors.Is(err, ErrClosed) {
		t.Errorf("Exec after Close = %v, want ErrClosed", err)
	}
	if _, err := srv.OpenSession("d-1", 24, 80, ""); !errors.Is(err, ErrClosed) {
		t.Errorf("OpenSession after Close = %v, want ErrClosed", err)
	}
}

func TestOperationsOnUnknownDaemon(t *testing.T) {
	srv, err := Start(Config{Bind: portNoDaemon})
	if err != nil {
		t.Fatalf("Start: %v", err)
	}
	defer srv.Close()

	// Exec distinguishes "not connected" by return code, so callers can map it to a
	// NotFound rather than an internal error.
	if _, err := srv.Exec("no-such-daemon", "true", time.Second); !errors.Is(err, ErrNoDaemon) {
		t.Errorf("Exec on unknown daemon = %v, want ErrNoDaemon", err)
	}

	// OpenSession cannot: the C side returns NULL for every failure, so only the message
	// distinguishes them. Asserted so a future ABI change that adds a code is noticed.
	_, err = srv.OpenSession("no-such-daemon", 24, 80, "")
	if err == nil {
		t.Fatal("OpenSession on an unknown daemon should fail")
	}
	if !strings.Contains(err.Error(), "not found") {
		t.Errorf("error should say the daemon was not found, got: %v", err)
	}
}

// Close must not free the handle while a call that copied it is still in C.
//
// Exec releases s.mu before calling, so nil'ing ptr does not protect the parked call —
// and sandd_server_free drops the tokio runtime that sandd_exec is parked on, making this
// a use-after-free of a running executor rather than a stale-handle read. Asserted
// through the inflight counter directly: reproducing the free is undefined behaviour,
// which a test cannot observe reliably (it may well pass while corrupting memory).
//
// Run this under -race and with CGO_LDFLAGS pointing at a debug/ASan archive to get more
// than the counter check.
func TestCloseWaitsForInflightCalls(t *testing.T) {
	srv, err := Start(Config{Bind: portInflight})
	if err != nil {
		t.Fatalf("Start: %v", err)
	}

	// Stand in for a call parked in C: acquire is exactly what Exec does before it
	// releases the lock, and the count is what Close has to respect.
	ptr, err := srv.acquire()
	if err != nil {
		t.Fatalf("acquire: %v", err)
	}
	if ptr == nil {
		t.Fatal("acquire returned a nil handle on a live server")
	}

	closed := make(chan error, 1)
	go func() { closed <- srv.Close() }()

	// Close must still be blocked while the call is outstanding. A poll rather than a
	// single sleep so the test does not depend on goroutine scheduling order.
	select {
	case <-closed:
		t.Fatal("Close returned while a call was still in flight; the handle was freed underneath it")
	case <-time.After(100 * time.Millisecond):
	}

	// Releasing lets Close finish — this is the ordering that makes the free safe.
	srv.release()

	select {
	case err := <-closed:
		if err != nil {
			t.Fatalf("Close: %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Close did not return after the in-flight call finished (counter leak?)")
	}

	// The waiting must not have cost idempotency or the post-Close contract.
	if err := srv.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
	if _, err := srv.Exec("d-1", "true", time.Second); !errors.Is(err, ErrClosed) {
		t.Errorf("Exec after Close = %v, want ErrClosed", err)
	}
}

// Session.Close has the same duty as Server.Close: Read parks in C holding its own copy
// of the handle, so freeing without waiting is a use-after-free.
//
// Driven through stubSession (see controller.go) because a real Session needs a connected
// daemon, and the handle must be non-nil or Close short-circuits as already-closed and
// never reaches the wait. What is under test is the ordering between the wait and the free.
func TestSessionCloseWaitsForAParkedRead(t *testing.T) {
	freed := make(chan struct{})
	sess := stubSession(func() { close(freed) })

	// A read parked in sandd_session_read: it has left s.mu and holds its own pointer
	// copy, which is precisely why nil'ing ptr is not enough.
	sess.inflight.Add(1)

	closed := make(chan error, 1)
	go func() { closed <- sess.Close() }()

	select {
	case <-closed:
		t.Fatal("Close returned while a read was parked; the handle was freed underneath it")
	case <-freed:
		t.Fatal("the handle was freed while a read was still parked on it")
	case <-time.After(100 * time.Millisecond):
	}

	sess.inflight.Done()

	select {
	case err := <-closed:
		if err != nil {
			t.Fatalf("Close: %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Close did not return after the read finished (counter leak?)")
	}

	// The free must have happened — waiting must not have skipped it.
	select {
	case <-freed:
	default:
		t.Error("Close returned without freeing the handle")
	}

	// Only a reader arriving AFTER Close sees ErrClosed; the parked one saw its own
	// result. Asserted because the doc comment used to claim the opposite.
	if _, err := sess.Read(make([]byte, ReadBufSize), time.Second); !errors.Is(err, ErrClosed) {
		t.Errorf("Read after Close = %v, want ErrClosed", err)
	}
	if err := sess.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
}

// A caller arriving after Close must be refused rather than joining the set Close is
// waiting on — otherwise Close could wait forever, or worse, return and then have a new
// call use the freed handle.
func TestAcquireAfterCloseIsRefused(t *testing.T) {
	srv, err := Start(Config{Bind: portCloseRace})
	if err != nil {
		t.Fatalf("Start: %v", err)
	}
	if err := srv.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	if _, err := srv.acquire(); !errors.Is(err, ErrClosed) {
		t.Errorf("acquire after Close = %v, want ErrClosed", err)
	}

	// Hammer it from several goroutines: every one must be refused, and none may leave
	// the counter incremented (a leak would wedge any later Wait).
	var wg sync.WaitGroup
	for range 16 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			if _, err := srv.acquire(); !errors.Is(err, ErrClosed) {
				t.Errorf("concurrent acquire after Close = %v, want ErrClosed", err)
			}
		}()
	}
	wg.Wait()

	// Wait returns immediately iff nothing leaked a count.
	done := make(chan struct{})
	go func() { srv.inflight.Wait(); close(done) }()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("inflight counter leaked: a refused acquire incremented it")
	}
}

// The error message is THREAD-LOCAL in Rust. cgo pins a goroutine to its OS thread only
// for the duration of a call, so concurrent failures on different goroutines must not
// bleed each other's messages — this asserts each caller sees its own.
func TestConcurrentErrorsDoNotCrossThreads(t *testing.T) {
	srv, err := Start(Config{Bind: portErrs})
	if err != nil {
		t.Fatalf("Start: %v", err)
	}
	defer srv.Close()

	const goroutines = 16
	var wg sync.WaitGroup
	errs := make([]error, goroutines)
	for i := range goroutines {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			// Each goroutine asks about a DISTINCT daemon id, so a message from another
			// goroutine is detectable by its content.
			id := "daemon-" + string(rune('a'+i))
			_, errs[i] = srv.Exec(id, "true", time.Second)
		}(i)
	}
	wg.Wait()

	for i, err := range errs {
		if !errors.Is(err, ErrNoDaemon) {
			t.Errorf("goroutine %d: got %v, want ErrNoDaemon", i, err)
			continue
		}
		want := "daemon-" + string(rune('a'+i))
		if !strings.Contains(err.Error(), want) {
			t.Errorf("goroutine %d: error names the wrong daemon (%v), want %q", i, err, want)
		}
	}
}

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

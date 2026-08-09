// The Go binding for the SandD controller.
//
// Lives HERE rather than in a consumer: it wraps this repo's C ABI (server/src/ffi.rs),
// so an ABI change and its binding move in one commit and break one build. A copy in
// Nebula would drift silently — the linker only catches a missing symbol, never a
// changed meaning.
module github.com/InftyAI/SandD/go

go 1.24

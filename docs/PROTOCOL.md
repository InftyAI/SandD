# SandD Protocol Specification

WebSocket-based JSON protocol for communication between agent and daemon.

## Protocol Versioning

SandD uses WebSocket subprotocol negotiation for versioning via the `Sec-WebSocket-Protocol` header:

**Client (Daemon) Request:**
```
GET /ws HTTP/1.1
Upgrade: websocket
Sec-WebSocket-Protocol: sandd.v1
```

**Server (Agent) Response:**
```
HTTP/1.1 101 Switching Protocols
Upgrade: websocket
Sec-WebSocket-Protocol: sandd.v1
```

**Current Version:** `sandd.v1`

**Benefits:**
- Protocol-native versioning mechanism
- Client can propose multiple versions: `Sec-WebSocket-Protocol: sandd.v1, sandd.v2`
- Server selects best supported version

## Connection Architecture

```
┌─────────────┐                    ┌─────────────┐
│   Agent     │                    │   Daemon    │
│  (Server)   │                    │  (Client)   │
└──────┬──────┘                    └──────┬──────┘
       │                                  │
       │◄─────── WebSocket Connect ───────┤
       │         (Daemon initiates)       │
       │                                  │
       │◄──────── Register ───────────────┤
       ├────────── RegisterAck ──────────►│
       │                                  │
       │◄──────── Heartbeat ──────────────┤ (every 30s)
       │                                  │
       ├─────── ExecuteCommand ──────────►│
       │◄────── CommandOutput ────────────┤
       │                                  │
```

**Key Design**: Daemon connects TO the agent (reverse connection), so no ports need to be exposed on the execution plane.

## Message Format

All messages are JSON with a `type` field indicating the message type:

```json
{
  "type": "exec",
  "request_id": "uuid-here",
  "command": "ls -la",
  "timeout_secs": 300,
  "env": {},
  "cwd": null
}
```

## Message Types

### Connection Management

#### Register
**Direction**: Daemon → Agent
**Purpose**: Daemon registers itself when connecting

```json
{
  "type": "register",
  "daemon_id": "worker-1",
  "metadata": {
    "hostname": "worker-01",
    "platform": "linux",
    "arch": "x86_64",
    "version": "0.1.0",
    "labels": {
      "region": "us-west",
      "env": "prod"
    }
  }
}
```

#### RegisterAck
**Direction**: Agent → Daemon
**Purpose**: Acknowledge successful registration

```json
{
  "type": "register_ack",
  "success": true,
  "message": "Successfully registered"
}
```

#### Heartbeat
**Direction**: Daemon → Agent
**Purpose**: Keep connection alive (sent every 30 seconds)

```json
{
  "type": "heartbeat"
}
```

#### Pong
**Direction**: Agent → Daemon
**Purpose**: Response to heartbeat (optional)

```json
{
  "type": "pong"
}
```

### Command Execution

#### ExecuteCommand
**Direction**: Agent → Daemon
**Purpose**: Execute a shell command

```json
{
  "type": "exec",
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "command": "python script.py",
  "timeout_secs": 300,
  "env": {
    "MY_VAR": "value"
  },
  "cwd": "/opt/app"
}
```

**Fields**:
- `request_id`: Unique identifier for tracking this request
- `command`: Shell command to execute
- `timeout_secs`: Maximum execution time (default: 300)
- `env`: Environment variables (optional)
- `cwd`: Working directory (optional)

#### CommandOutput
**Direction**: Daemon → Agent
**Purpose**: Return command execution results

```json
{
  "type": "command_output",
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "stdout": "output text...",
  "stderr": "",
  "exit_code": 0,
  "duration_ms": 1234
}
```

#### CommandError
**Direction**: Daemon → Agent
**Purpose**: Report command execution error

```json
{
  "type": "command_error",
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "error": "command not found"
}
```

### Interactive Session (PTY)

#### StartSession
**Direction**: Agent → Daemon
**Purpose**: Start an interactive session

```json
{
  "type": "start_session",
  "session_id": "550e8400-e29b-41d4-a716-446655440001",
  "rows": 24,
  "cols": 80,
  "term": "xterm-256color"
}
```

#### SessionStarted
**Direction**: Daemon → Agent
**Purpose**: Acknowledge session started

```json
{
  "type": "session_started",
  "session_id": "550e8400-e29b-41d4-a716-446655440001",
  "success": true,
  "error": null
}
```

#### SessionInput
**Direction**: Agent → Daemon
**Purpose**: Send user input to session

```json
{
  "type": "session_input",
  "session_id": "550e8400-e29b-41d4-a716-446655440001",
  "data": "bHMgLWxhCg=="
}
```

**Note**: `data` is base64-encoded bytes

#### SessionOutput
**Direction**: Daemon → Agent
**Purpose**: Stream session output back to agent

```json
{
  "type": "session_output",
  "session_id": "550e8400-e29b-41d4-a716-446655440001",
  "data": "ZmlsZTEgIGZpbGUyICBmaWxlMwo="
}
```

**Note**: `data` is base64-encoded bytes

#### SessionResize
**Direction**: Agent → Daemon
**Purpose**: Resize terminal window

```json
{
  "type": "session_resize",
  "session_id": "550e8400-e29b-41d4-a716-446655440001",
  "rows": 50,
  "cols": 120
}
```

#### SessionClose
**Direction**: Agent → Daemon
**Purpose**: Close session

```json
{
  "type": "session_close",
  "session_id": "550e8400-e29b-41d4-a716-446655440001"
}
```

#### SessionExit
**Direction**: Daemon → Agent
**Purpose**: Session terminated

```json
{
  "type": "session_exit",
  "session_id": "550e8400-e29b-41d4-a716-446655440001",
  "exit_code": 0
}
```

### File Transfer

#### FileUploadStart
**Direction**: Agent → Daemon
**Purpose**: Begin uploading a file to daemon

```json
{
  "type": "file_upload_start",
  "transfer_id": "550e8400-e29b-41d4-a716-446655440002",
  "path": "/etc/app/config.yaml",
  "total_size": 4096,
  "mode": 420
}
```

**Fields**:
- `mode`: Unix file permissions (e.g., 420 = 0644 octal), optional

#### FileUploadChunk
**Direction**: Agent → Daemon
**Purpose**: Send file data chunk

```json
{
  "type": "file_upload_chunk",
  "transfer_id": "550e8400-e29b-41d4-a716-446655440002",
  "data": "Y29udGVudCBoZXJl...",
  "offset": 0
}
```

**Note**:
- `data` is base64-encoded bytes
- Chunks are typically 64KB
- `offset` tracks position in file

#### FileUploadComplete
**Direction**: Daemon → Agent
**Purpose**: Acknowledge file upload completion

```json
{
  "type": "file_upload_complete",
  "transfer_id": "550e8400-e29b-41d4-a716-446655440002",
  "success": true,
  "error": null
}
```

#### FileDownloadStart
**Direction**: Agent → Daemon
**Purpose**: Request file download from daemon

```json
{
  "type": "file_download_start",
  "transfer_id": "550e8400-e29b-41d4-a716-446655440003",
  "path": "/var/log/app.log"
}
```

#### FileDownloadChunk
**Direction**: Daemon → Agent
**Purpose**: Send file data chunk

```json
{
  "type": "file_download_chunk",
  "transfer_id": "550e8400-e29b-41d4-a716-446655440003",
  "data": "bG9nIGRhdGEgaGVyZQ==",
  "offset": 0,
  "is_last": false
}
```

**Note**:
- `is_last`: true on final chunk
- Agent buffers chunks until `is_last = true`

#### FileDownloadError
**Direction**: Daemon → Agent
**Purpose**: Report file download error

```json
{
  "type": "file_download_error",
  "transfer_id": "550e8400-e29b-41d4-a716-446655440003",
  "error": "file not found"
}
```

### Error Handling

#### Error
**Direction**: Either
**Purpose**: Generic error message

```json
{
  "type": "error",
  "message": "connection lost",
  "recoverable": false
}
```

## Communication Patterns

### Request/Response (Command Execution)

1. Agent generates unique `request_id`
2. Agent registers oneshot channel for this request
3. Agent sends `ExecuteCommand` message
4. Daemon executes and sends back `CommandOutput`
5. Agent resolves channel, Python receives result

**Concurrency**: Multiple commands can execute in parallel

### Streaming (Interactive Sessions)

1. Agent generates unique `session_id`
2. Agent registers mpsc channel for this session
3. Agent sends `StartSession` message
4. Daemon starts PTY and begins streaming output
5. Agent sends `SessionInput` as user types
6. Daemon sends `SessionOutput` continuously
7. Session ends with `SessionExit`

**Concurrency**: Multiple sessions per daemon supported

### Chunked Transfer (File Download)

1. Agent generates unique `transfer_id`
2. Agent sends `FileDownloadStart`
3. Daemon reads file and sends multiple `FileDownloadChunk` messages
4. Agent buffers chunks in DashMap
5. Last chunk has `is_last = true`
6. Agent assembles complete file from chunks

**Chunk Size**: 64KB (configurable)

## Connection Lifecycle

```
1. Daemon starts → connects to agent WebSocket endpoint
2. Daemon sends Register message
3. Agent creates DaemonConnection and stores in registry
4. Agent sends RegisterAck
5. Daemon enters main loop:
   - Sends Heartbeat every 30s
   - Listens for commands from agent
   - Executes commands and sends results
6. On disconnect:
   - Agent detects closed connection
   - Registry removes daemon
   - All pending commands fail
   - Terminate
```

## Heartbeat & Connection Monitoring

- **Heartbeat interval**: 30 seconds (daemon → agent)
- **Stale timeout**: 90 seconds (agent checks every 30s)
- **Auto-reconnect**: Daemon automatically reconnects if connection drops

## Security Considerations

1. **No authentication in MVP**: Add token-based auth in production
2. **Use WSS (TLS)**: Encrypt all communication in production
3. **Command validation**: Agent should validate/sanitize commands
4. **File path validation**: Prevent directory traversal attacks
5. **Resource limits**: Implement per-daemon quotas

## Implementation Details

See `server/src/protocol.rs` for the complete Rust implementation using serde for JSON serialization.

Binary data (session I/O, file chunks) is base64-encoded for JSON compatibility.

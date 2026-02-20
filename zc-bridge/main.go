package main

import (
    "bytes"
    "encoding/json"
    "fmt"
    "log"
    "net/http"
    "os"
    "sync"
    "time"

    "github.com/gorilla/websocket"
)

var (
    addr        = ":18789"
    zeroclawURL = getenv("ZEROCLAW_URL", "http://zeroclaw:3000/webhook")
    upgrader    = websocket.Upgrader{CheckOrigin: func(r *http.Request) bool { return true }}
)

type Frame struct {
    Type    string          `json:"type"`
    ID      string          `json:"id,omitempty"`
    Method  string          `json:"method,omitempty"`
    Params  json.RawMessage `json:"params,omitempty"`
    Ok      bool            `json:"ok,omitempty"`
    Payload json.RawMessage `json:"payload,omitempty"`
    Error   *ErrPayload     `json:"error,omitempty"`
    Event   string          `json:"event,omitempty"`
    Seq     int64           `json:"seq,omitempty"`
}

type ErrPayload struct {
    Code    string `json:"code"`
    Message string `json:"message"`
}

type Session struct {
    Key       string    `json:"key"`
    Status    string    `json:"status"`
    Model     string    `json:"model"`
    CreatedAt time.Time `json:"createdAt"`
}

// ContentBlock represents a content block in ClawSuite message format
type ContentBlock struct {
    Type string `json:"type"` // "text"
    Text string `json:"text"`
}

// ChatMessage represents a single message in history
type ChatMessage struct {
    Role    string         `json:"role"`
    Content []ContentBlock `json:"content"` // Array of content blocks
}

var (
    sessions      = map[string]*Session{}
    sessionsMu    sync.Mutex
    seq           int64
    chatHistory   = map[string][]ChatMessage{} // sessionKey -> messages
    chatHistoryMu sync.Mutex
)

func getenv(k, d string) string {
    v := os.Getenv(k)
    if v == "" {
        return d
    }
    return v
}

func mustJSON(v any) json.RawMessage {
    b, _ := json.Marshal(v)
    return b
}

func nextSeq() int64 {
    seq++
    return seq
}

// addMessage appends a message to the chat history for a session
func addMessage(sessionKey, role, text string) {
    chatHistoryMu.Lock()
    defer chatHistoryMu.Unlock()
    chatHistory[sessionKey] = append(chatHistory[sessionKey], ChatMessage{
        Role:    role,
        Content: []ContentBlock{{Type: "text", Text: text}},
    })
    if os.Getenv("ZC_BRIDGE_DEBUG") == "1" {
        preview := text
        if len(preview) > 80 {
            preview = preview[:80]
        }
        log.Printf("[history] add sessionKey=%s role=%s len=%d preview=%q", sessionKey, role, len(text), preview)
    }
}

// getMessages returns messages for a session (up to limit)
func getMessages(sessionKey string, limit int) []ChatMessage {
    chatHistoryMu.Lock()
    defer chatHistoryMu.Unlock()
    msgs := chatHistory[sessionKey]
    if limit > 0 && len(msgs) > limit {
        return msgs[len(msgs)-limit:]
    }
    return msgs
}

func safeWriteJSON(ws *websocket.Conn, mu *sync.Mutex, v any) error {
    mu.Lock()
    defer mu.Unlock()
    return ws.WriteJSON(v)
}

func safeWriteControl(ws *websocket.Conn, mu *sync.Mutex, messageType int, data []byte, deadline time.Time) error {
    mu.Lock()
    defer mu.Unlock()
    return ws.WriteControl(messageType, data, deadline)
}

func main() {
    http.HandleFunc("/", handleWS)
    log.Println("zc-bridge listening on", addr)
    log.Fatal(http.ListenAndServe(addr, nil))
}

func handleWS(w http.ResponseWriter, r *http.Request) {
    ws, err := upgrader.Upgrade(w, r, nil)
    if err != nil {
        log.Println(err)
        return
    }
    defer ws.Close()

    writeMu := &sync.Mutex{}

    log.Println("client connected")

    safeWriteJSON(ws, writeMu, Frame{
        Type:  "event",
        Event: "connect.challenge",
        Payload: mustJSON(map[string]any{
            "nonce": time.Now().UnixNano(),
        }),
    })

    for {
        var f Frame
        if err := ws.ReadJSON(&f); err != nil {
            return
        }
        if f.Type == "req" && f.Method == "connect" {
            safeWriteJSON(ws, writeMu, Frame{Type: "res", ID: f.ID, Ok: true})
            break
        }
    }

    log.Println("gateway authenticated")

    go heartbeat(ws, writeMu)

    for {
        var f Frame
        if err := ws.ReadJSON(&f); err != nil {
            return
        }
        if f.Type != "req" {
            continue
        }
        go handleRPC(ws, writeMu, f)
    }
}

func heartbeat(ws *websocket.Conn, writeMu *sync.Mutex) {
    t := time.NewTicker(30 * time.Second)
    for range t.C {
        safeWriteControl(ws, writeMu, websocket.PingMessage, []byte("ping"), time.Now().Add(2*time.Second))
    }
}

func handleRPC(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    if os.Getenv("ZC_BRIDGE_DEBUG") == "1" {
        log.Printf("[rpc] method=%s id=%s paramsLen=%d", f.Method, f.ID, len(f.Params))
    }

    // Handle all known methods locally or via ZeroClaw
    switch f.Method {
    case "sessions.list":
        handleSessionsList(ws, writeMu, f)
    case "models.list":
        handleModelsList(ws, writeMu, f)
    case "sessions.patch":
        handleSessionsPatch(ws, writeMu, f)
    case "sessions.resolve":
        handleSessionsResolve(ws, writeMu, f)
    case "sessions.status":
        handleSessionsStatus(ws, writeMu, f)
    case "session.status":
        handleSessionStatus(ws, writeMu, f)
    case "sessions.usage":
        handleSessionsUsage(ws, writeMu, f)
    case "usage.cost":
        handleUsageCost(ws, writeMu, f)
    case "usage.status":
        handleUsageStatus(ws, writeMu, f)
    case "status":
        handleStatus(ws, writeMu, f)
    case "cron.list", "cron.jobs.list", "scheduler.jobs.list":
        handleEmptyList(ws, writeMu, f)
    case "chat.history":
        handleChatHistory(ws, writeMu, f)
    case "sessions.send", "chat.send":
        handleZeroClawForward(ws, writeMu, f)
    default:
        sendError(ws, writeMu, f.ID, "unsupported method: "+f.Method)
    }
}

// --- Local RPC handlers ---

func handleSessionsList(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    sessionsMu.Lock()
    list := make([]*Session, 0, len(sessions))
    for _, s := range sessions {
        list = append(list, s)
    }
    sessionsMu.Unlock()

    safeWriteJSON(ws, writeMu, Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(map[string]any{"sessions": list}),
    })
}

func handleModelsList(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    safeWriteJSON(ws, writeMu, Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(map[string]any{
            "models": []string{"kimi-k2.5"},
        }),
    })
}

func handleSessionsPatch(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    safeWriteJSON(ws, writeMu, Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(map[string]any{}),
    })
}

func handleSessionsResolve(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    // Parse params to get key
    var params struct {
        Key string `json:"key"`
    }
    key := "main"
    if len(f.Params) > 0 {
        if err := json.Unmarshal(f.Params, &params); err == nil && params.Key != "" {
            key = params.Key
        }
    }

    safeWriteJSON(ws, writeMu, Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(map[string]any{"ok": true, "key": key}),
    })
}

func handleSessionsStatus(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    safeWriteJSON(ws, writeMu, Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(map[string]any{
            "sessions": []any{},
        }),
    })
}

func handleSessionStatus(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    safeWriteJSON(ws, writeMu, Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(map[string]any{
            "status": "idle",
        }),
    })
}

func handleSessionsUsage(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    safeWriteJSON(ws, writeMu, Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(map[string]any{
            "sessions": []any{},
            "totalInputTokens":  0,
            "totalOutputTokens": 0,
            "totalCostUsd":      0.0,
        }),
    })
}

func handleUsageCost(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    safeWriteJSON(ws, writeMu, Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(map[string]any{
            "totalCostUsd":      0.0,
            "totalInputTokens":  0,
            "totalOutputTokens": 0,
            "byModel":           map[string]any{},
        }),
    })
}

func handleUsageStatus(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    safeWriteJSON(ws, writeMu, Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(map[string]any{
            "available": true,
        }),
    })
}

func handleStatus(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    safeWriteJSON(ws, writeMu, Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(map[string]any{
            "status":  "ok",
            "version": "zc-bridge-1.0",
        }),
    })
}

func handleEmptyList(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    safeWriteJSON(ws, writeMu, Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(map[string]any{
            "jobs": []any{},
        }),
    })
}

func handleChatHistory(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    // Parse params
    var params struct {
        SessionKey string `json:"sessionKey"`
        Limit      int    `json:"limit"`
    }
    sessionKey := "main"
    limit := 200
    if len(f.Params) > 0 {
        if err := json.Unmarshal(f.Params, &params); err == nil {
            if params.SessionKey != "" {
                sessionKey = params.SessionKey
            }
            if params.Limit > 0 {
                limit = params.Limit
            }
        }
    }

    msgs := getMessages(sessionKey, limit)

    if os.Getenv("ZC_BRIDGE_DEBUG") == "1" {
        log.Printf("[history] get sessionKey=%s limit=%d returning=%d", sessionKey, limit, len(msgs))
        for i, m := range msgs {
            // Log first content block text
            var preview string
            if len(m.Content) > 0 && m.Content[0].Type == "text" {
                preview = m.Content[0].Text
                if len(preview) > 60 {
                    preview = preview[:60]
                }
            }
            log.Printf("[history] msg[%d] role=%s content=[{type:text text:%q}]", i, m.Role, preview)
        }
    }

    safeWriteJSON(ws, writeMu, Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(map[string]any{
            "sessionKey": sessionKey,
            "messages":   msgs,
        }),
    })
}

func handleZeroClawForward(ws *websocket.Conn, writeMu *sync.Mutex, f Frame) {
    if os.Getenv("ZC_BRIDGE_DEBUG") == "1" {
        log.Printf("[forward] method=%s id=%s", f.Method, f.ID)
    }

    // Parse params
    var params struct {
        SessionKey     string `json:"sessionKey"`
        Message        string `json:"message"`
        IdempotencyKey string `json:"idempotencyKey"`
    }
    if len(f.Params) > 0 {
        if err := json.Unmarshal(f.Params, &params); err != nil {
            sendError(ws, writeMu, f.ID, "invalid params: "+err.Error())
            return
        }
    }
    if params.Message == "" {
        sendError(ws, writeMu, f.ID, "missing params.message")
        return
    }

    // Determine sessionKey (fallback to "main")
    sessionKey := params.SessionKey
    if sessionKey == "" {
        sessionKey = "main"
    }

    // Determine runId
    runId := params.IdempotencyKey
    if runId == "" {
        runId = fmt.Sprintf("run_%d", time.Now().UnixNano())
    }

    // Store user message in history
    addMessage(sessionKey, "user", params.Message)

    // Build ZeroClaw webhook payload
    body := map[string]any{
        "message": params.Message,
    }

    j, _ := json.Marshal(body)

    req, err := http.NewRequest("POST", zeroclawURL, bytes.NewReader(j))
    if err != nil {
        log.Printf("[forward] error: %s", err)
        sendError(ws, writeMu, f.ID, err.Error())
        return
    }
    req.Header.Set("Content-Type", "application/json")

    token := os.Getenv("ZEROCLAW_BEARER_TOKEN")
    if token != "" {
        req.Header.Set("Authorization", "Bearer "+token)
    }

    resp, err := http.DefaultClient.Do(req)
    if err != nil {
        log.Printf("[forward] error: %s", err)
        sendError(ws, writeMu, f.ID, err.Error())
        return
    }
    defer resp.Body.Close()

    if os.Getenv("ZC_BRIDGE_DEBUG") == "1" {
        log.Printf("[forward] zeroclaw status=%d", resp.StatusCode)
    }

    // Read full response body
    b := make([]byte, 0)
    if resp.Body != nil {
        buf := make([]byte, 4096)
        for {
            n, readErr := resp.Body.Read(buf)
            if n > 0 {
                b = append(b, buf[:n]...)
            }
            if readErr != nil {
                break
            }
        }
    }

    // Check for error status
    if resp.StatusCode >= 400 {
        sendError(ws, writeMu, f.ID, fmt.Sprintf("zeroclaw %d: %s", resp.StatusCode, string(b)))
        return
    }

    // Extract assistant text from response
    assistantText := ""
    var m map[string]any
    if json.Unmarshal(b, &m) == nil {
        // Try common keys
        for _, key := range []string{"response", "message", "text", "output"} {
            if v, ok := m[key]; ok {
                if s, ok := v.(string); ok && s != "" {
                    assistantText = s
                    break
                }
            }
        }
        // Try nested data.text
        if assistantText == "" {
            if data, ok := m["data"].(map[string]any); ok {
                if v, ok := data["text"]; ok {
                    if s, ok := v.(string); ok && s != "" {
                        assistantText = s
                    }
                }
            }
        }
    }
    // Fallback to raw string if not found
    if assistantText == "" {
        assistantText = string(b)
    }
    if assistantText == "" {
        assistantText = "(empty response)"
    }

    // Store assistant message in history
    addMessage(sessionKey, "assistant", assistantText)

    // Send RPC response frame first
    safeWriteJSON(ws, writeMu, Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(map[string]any{"runId": runId}),
    })

    // Emit agent event
    safeWriteJSON(ws, writeMu, Frame{
        Type:  "event",
        Event: "agent",
        Seq:   nextSeq(),
        Payload: mustJSON(map[string]any{
            "runId":      runId,
            "sessionKey": sessionKey,
            "stream":     "assistant",
            "data": map[string]any{
                "text": assistantText,
            },
        }),
    })

    // Emit chat final event
    safeWriteJSON(ws, writeMu, Frame{
        Type:  "event",
        Event: "chat",
        Seq:   nextSeq(),
        Payload: mustJSON(map[string]any{
            "runId":      runId,
            "sessionKey": sessionKey,
            "state":      "final",
            "message": map[string]any{
                "role": "assistant",
                "content": []ContentBlock{{Type: "text", Text: assistantText}},
            },
        }),
    })
}

func sendError(ws *websocket.Conn, writeMu *sync.Mutex, id, msg string) {
    safeWriteJSON(ws, writeMu, Frame{
        Type: "res",
        ID:   id,
        Ok:   false,
        Error: &ErrPayload{
            Code:    "bridge_error",
            Message: msg,
        },
    })
}
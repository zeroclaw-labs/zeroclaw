package main

import (
    "bytes"
    "encoding/json"
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

var (
    sessions   = map[string]*Session{}
    sessionsMu sync.Mutex
    seq        int64
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

    log.Println("client connected")

    ws.WriteJSON(Frame{
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
            ws.WriteJSON(Frame{Type: "res", ID: f.ID, Ok: true})
            break
        }
    }

    log.Println("gateway authenticated")

    go heartbeat(ws)

    for {
        var f Frame
        if err := ws.ReadJSON(&f); err != nil {
            return
        }
        if f.Type != "req" {
            continue
        }
        go handleRPC(ws, f)
    }
}

func heartbeat(ws *websocket.Conn) {
    t := time.NewTicker(30 * time.Second)
    for range t.C {
        ws.WriteControl(websocket.PingMessage, []byte("ping"), time.Now().Add(2*time.Second))
    }
}

func handleRPC(ws *websocket.Conn, f Frame) {
    switch f.Method {

    case "sessions.list":
        sessionsMu.Lock()
        list := make([]*Session, 0, len(sessions))
        for _, s := range sessions {
            list = append(list, s)
        }
        sessionsMu.Unlock()

        ws.WriteJSON(Frame{
            Type:    "res",
            ID:      f.ID,
            Ok:      true,
            Payload: mustJSON(map[string]any{"sessions": list}),
        })

    case "models.list":
        ws.WriteJSON(Frame{
            Type:    "res",
            ID:      f.ID,
            Ok:      true,
            Payload: mustJSON(map[string]any{
                "models": []string{"kimi-k2.5"},
            }),
        })

    default:
        handleZeroClawForward(ws, f)
    }
}

func handleZeroClawForward(ws *websocket.Conn, f Frame) {

    body := map[string]any{
        "method": f.Method,
        "params": json.RawMessage(f.Params),
    }

    j, _ := json.Marshal(body)

    req, err := http.NewRequest("POST", zeroclawURL, bytes.NewReader(j))
    if err != nil {
        sendError(ws, f.ID, err.Error())
        return
    }
    req.Header.Set("Content-Type", "application/json")

    token := os.Getenv("ZEROCLAW_BEARER_TOKEN")
    if token != "" {
        req.Header.Set("Authorization", "Bearer "+token)
    }

    resp, err := http.DefaultClient.Do(req)
    if err != nil {
        sendError(ws, f.ID, err.Error())
        return
    }
    defer resp.Body.Close()

    var payload any
    json.NewDecoder(resp.Body).Decode(&payload)

    ws.WriteJSON(Frame{
        Type:    "res",
        ID:      f.ID,
        Ok:      true,
        Payload: mustJSON(payload),
    })

    ws.WriteJSON(Frame{
        Type:    "event",
        Event:   "session.updated",
        Seq:     nextSeq(),
        Payload: mustJSON(payload),
    })
}

func sendError(ws *websocket.Conn, id, msg string) {
    ws.WriteJSON(Frame{
        Type: "res",
        ID:   id,
        Ok:   false,
        Error: &ErrPayload{
            Code:    "bridge_error",
            Message: msg,
        },
    })
}
package main

import (
  "bufio"
  "bytes"
  "context"
  "encoding/json"
  "fmt"
  "io"
  "log"
  "net/http"
  "net/url"
  "os"
  "strings"
  "sync"
  "time"

  "github.com/gorilla/websocket"
)

// sseWriter synchronizes writes to an http.ResponseWriter so that the
// keepalive goroutine and the main relay loop never interleave output.
type sseWriter struct {
  mu      sync.Mutex
  w       http.ResponseWriter
  flusher http.Flusher
}

func (s *sseWriter) WriteEvent(data []byte) {
  s.mu.Lock()
  defer s.mu.Unlock()
  _, _ = s.w.Write([]byte("data: "))
  _, _ = s.w.Write(data)
  _, _ = s.w.Write([]byte("\n\n"))
  s.flusher.Flush()
}

func (s *sseWriter) WriteRaw(line []byte, flush bool) {
  s.mu.Lock()
  defer s.mu.Unlock()
  _, _ = s.w.Write(line)
  if flush {
    s.flusher.Flush()
  }
}

func (s *sseWriter) WriteKeepalive() {
  s.mu.Lock()
  defer s.mu.Unlock()
  _, _ = s.w.Write([]byte(": keepalive\n\n"))
  s.flusher.Flush()
}

type rpcRequest struct {
  JSONRPC string          `json:"jsonrpc"`
  Method  string          `json:"method"`
  Params  json.RawMessage `json:"params"`
  ID      any             `json:"id"`
}

type rpcError struct {
  Code    int    `json:"code"`
  Message string `json:"message"`
}

type rpcResponse struct {
  JSONRPC string          `json:"jsonrpc"`
  Result  any             `json:"result,omitempty"`
  Error   *rpcError       `json:"error,omitempty"`
  ID      any             `json:"id"`
}

type sendParams struct {
  Recipient []string `json:"recipient"`
  GroupID   string   `json:"groupId"`
  Message   string   `json:"message"`
  Account   string   `json:"account"`
}

var (
  listenAddr     = envDefault("LISTEN_ADDR", ":8686")
  bridgeURL      = strings.TrimRight(envDefault("BRIDGE_BASE_URL", "http://127.0.0.1:8080"), "/")
  pollTimeout    = envDefault("POLL_TIMEOUT_SECONDS", "20")
  defaultAccount = envDefault("SIGNAL_ACCOUNT", "")
  httpClient     = &http.Client{Timeout: 45 * time.Second}
  sseClient      = &http.Client{} // no timeout: SSE streams are long-lived
)

func envDefault(k, v string) string {
  val := strings.TrimSpace(os.Getenv(k))
  if val == "" {
    return v
  }
  return val
}

func writeRPC(w http.ResponseWriter, status int, resp rpcResponse) {
  w.Header().Set("Content-Type", "application/json")
  w.WriteHeader(status)
  _ = json.NewEncoder(w).Encode(resp)
}

func bridgeRequest(ctx context.Context, method, path string, body io.Reader, headers map[string]string) (*http.Response, []byte, error) {
  req, err := http.NewRequestWithContext(ctx, method, bridgeURL+path, body)
  if err != nil {
    return nil, nil, err
  }
  for k, v := range headers {
    req.Header.Set(k, v)
  }

  resp, err := httpClient.Do(req)
  if err != nil {
    return nil, nil, err
  }
  defer resp.Body.Close()

  b, err := io.ReadAll(resp.Body)
  if err != nil {
    return resp, nil, err
  }
  return resp, b, nil
}

func handleCheck(w http.ResponseWriter, r *http.Request) {
  ctx, cancel := context.WithTimeout(r.Context(), 10*time.Second)
  defer cancel()

  resp, body, err := bridgeRequest(ctx, http.MethodGet, "/v1/about", nil, nil)
  if err != nil {
    http.Error(w, err.Error(), http.StatusBadGateway)
    return
  }
  if resp.StatusCode/100 != 2 {
    http.Error(w, string(body), http.StatusBadGateway)
    return
  }

  w.Header().Set("Content-Type", "application/json")
  _, _ = w.Write([]byte(`{"ok":true}`))
}

func toSSEPayload(item json.RawMessage) ([]byte, bool) {
  var obj map[string]any
  if err := json.Unmarshal(item, &obj); err != nil {
    return nil, false
  }

  if _, ok := obj["envelope"]; ok {
    out, err := json.Marshal(obj)
    return out, err == nil
  }

  outObj := map[string]any{"envelope": obj}
  out, err := json.Marshal(outObj)
  return out, err == nil
}

// fetchAttachmentData calls the signal-cli daemon's getAttachment JSON-RPC
// method and returns the Base64-encoded attachment content.
func fetchAttachmentData(ctx context.Context, account, attachmentID, recipient string) (string, error) {
	params := map[string]any{
		"id":      attachmentID,
		"account": account,
	}
	if recipient != "" {
		params["recipient"] = recipient
	}
	paramsJSON, _ := json.Marshal(params)

	rpcReq := rpcRequest{
		JSONRPC: "2.0",
		Method:  "getAttachment",
		Params:  json.RawMessage(paramsJSON),
		ID:      1,
	}
	buf, _ := json.Marshal(rpcReq)

	resp, body, err := bridgeRequest(ctx, http.MethodPost, "/api/v1/rpc",
		bytes.NewReader(buf), map[string]string{"Content-Type": "application/json"})
	if err != nil {
		return "", fmt.Errorf("getAttachment request: %w", err)
	}
	if resp.StatusCode/100 != 2 {
		return "", fmt.Errorf("getAttachment status=%d body=%s", resp.StatusCode, strings.TrimSpace(string(body)))
	}

	var rpcResp struct {
		Result any       `json:"result"`
		Error  *rpcError `json:"error"`
	}
	if err := json.Unmarshal(body, &rpcResp); err != nil {
		return "", fmt.Errorf("getAttachment unmarshal: %w", err)
	}
	if rpcResp.Error != nil {
		return "", fmt.Errorf("getAttachment rpc error: %s", rpcResp.Error.Message)
	}

	switch v := rpcResp.Result.(type) {
	case string:
		return v, nil
	case map[string]any:
		if data, ok := v["data"].(string); ok {
			return data, nil
		}
	}
	return "", fmt.Errorf("getAttachment: unexpected result type %T", rpcResp.Result)
}

// enrichAttachmentsInDataMessage fetches binary data for each attachment in a
// dataMessage map and injects it as a "data" field.
func enrichAttachmentsInDataMessage(ctx context.Context, account, source string, dataMsg map[string]any) {
	attachments, _ := dataMsg["attachments"].([]any)
	if len(attachments) == 0 {
		log.Printf("enrichAttachments: no attachments in dataMessage (keys=%v)", mapKeys(dataMsg))
		return
	}
	log.Printf("enrichAttachments: found %d attachment(s)", len(attachments))
	for i, att := range attachments {
		attMap, ok := att.(map[string]any)
		if !ok {
			log.Printf("enrichAttachments: attachment[%d] not a map, type=%T", i, att)
			continue
		}
		id, _ := attMap["id"].(string)
		contentType, _ := attMap["contentType"].(string)
		if id == "" {
			log.Printf("enrichAttachments: attachment[%d] has no id (keys=%v)", i, mapKeys(attMap))
			continue
		}
		log.Printf("enrichAttachments: fetching attachment[%d] id=%s contentType=%s", i, id, contentType)
		data, err := fetchAttachmentData(ctx, account, id, source)
		if err != nil {
			log.Printf("enrichAttachments: id=%s err=%v", id, err)
			continue
		}
		log.Printf("enrichAttachments: fetched attachment[%d] id=%s len=%d", i, id, len(data))
		attMap["data"] = data
		attachments[i] = attMap
	}
}

func mapKeys(m map[string]any) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	return keys
}

// enrichAttachments parses a message envelope, fetches attachment data from
// the signal-cli daemon, and returns the enriched JSON.
func enrichAttachments(ctx context.Context, account string, raw json.RawMessage) json.RawMessage {
	var obj map[string]any
	if err := json.Unmarshal(raw, &obj); err != nil {
		return raw
	}

	// The envelope may be at the top level (REST / polling responses) or
	// nested inside "params" (JSON-RPC notifications from SSE/WebSocket).
	envelope, _ := obj["envelope"].(map[string]any)
	if envelope == nil {
		if params, ok := obj["params"].(map[string]any); ok {
			envelope, _ = params["envelope"].(map[string]any)
		}
	}
	if envelope == nil {
		return raw
	}
	log.Printf("enrichAttachments: processing envelope from=%v", envelope["sourceNumber"])

	source, _ := envelope["sourceNumber"].(string)

	// Direct messages.
	if dm, ok := envelope["dataMessage"].(map[string]any); ok {
		enrichAttachmentsInDataMessage(ctx, account, source, dm)
	}

	// Sync messages (messages sent from another linked device).
	if sm, ok := envelope["syncMessage"].(map[string]any); ok {
		if sent, ok := sm["sentMessage"].(map[string]any); ok {
			if dm, ok := sent["dataMessage"].(map[string]any); ok {
				enrichAttachmentsInDataMessage(ctx, account, source, dm)
			}
		}
	}

	// Normalize receipt type: signal-cli uses isDelivery/isRead booleans
	// but zeroclaw expects a "type" string field.
	if rm, ok := envelope["receiptMessage"].(map[string]any); ok {
		if _, hasType := rm["type"]; !hasType {
			if isRead, _ := rm["isRead"].(bool); isRead {
				rm["type"] = "READ"
			} else if isDel, _ := rm["isDelivery"].(bool); isDel {
				rm["type"] = "DELIVERY"
			} else if isViewed, _ := rm["isViewed"].(bool); isViewed {
				rm["type"] = "VIEWED"
			}
		}
	}

	enriched, err := json.Marshal(obj)
	if err != nil {
		return raw
	}
	return enriched
}

func handleEvents(w http.ResponseWriter, r *http.Request) {
  account := strings.TrimSpace(r.URL.Query().Get("account"))
  if account == "" {
    account = defaultAccount
  }
  if account == "" {
    http.Error(w, "missing account query parameter", http.StatusBadRequest)
    return
  }

  flusher, ok := w.(http.Flusher)
  if !ok {
    http.Error(w, "streaming unsupported", http.StatusInternalServerError)
    return
  }

  w.Header().Set("Content-Type", "text/event-stream")
  w.Header().Set("Cache-Control", "no-cache")
  w.Header().Set("Connection", "keep-alive")
  w.WriteHeader(http.StatusOK)

  sw := &sseWriter{w: w, flusher: flusher}

  // Initial comment to ensure clients treat this as an open SSE stream.
  sw.WriteRaw([]byte(": connected\n\n"), true)

  log.Printf("events stream opened account=%s remote=%s", account, r.RemoteAddr)

  // SSE is the primary transport. On clean disconnect (daemon restart,
  // upstream reconnect) retry immediately with a brief backoff instead of
  // cascading through websocket/polling which adds 40+ seconds of blackout.
  consecutiveSSEFailures := 0
  for {
    if r.Context().Err() != nil {
      log.Printf("events stream closed account=%s", account)
      return
    }

    err := streamEventsFromSSEEndpoint(r.Context(), account, sw)
    if r.Context().Err() != nil {
      log.Printf("events stream closed account=%s", account)
      return
    }

    if err == nil {
      // Clean disconnect (EOF): daemon likely restarted. Retry SSE quickly.
      consecutiveSSEFailures = 0
      log.Printf("events sse stream ended cleanly account=%s; reconnecting in 1s", account)
      time.Sleep(1 * time.Second)
      continue
    }

    consecutiveSSEFailures++
    log.Printf("events sse failed account=%s err=%v (consecutive=%d)", account, err, consecutiveSSEFailures)

    // Only fall through to websocket/polling after repeated SSE failures.
    if consecutiveSSEFailures < 3 {
      time.Sleep(time.Duration(consecutiveSSEFailures) * time.Second)
      continue
    }

    // SSE is persistently broken; try websocket then polling.
    if wsErr := streamEventsOverWebsocket(r.Context(), account, sw); wsErr != nil {
      log.Printf("events websocket failed account=%s err=%v; falling back to polling", account, wsErr)
      if pollErr := streamEventsByPolling(r.Context(), account, sw); pollErr != nil {
        log.Printf("events polling failed account=%s err=%v", account, pollErr)
      }
    }
    if r.Context().Err() != nil {
      log.Printf("events stream closed account=%s", account)
      return
    }

    // Reset counter so we try SSE again at the top of the loop.
    consecutiveSSEFailures = 0
    time.Sleep(2 * time.Second)
  }
}

func streamEventsFromSSEEndpoint(ctx context.Context, account string, sw *sseWriter) error {
  sseURL := fmt.Sprintf("%s/api/v1/events?account=%s", bridgeURL, url.QueryEscape(account))
  req, err := http.NewRequestWithContext(ctx, http.MethodGet, sseURL, nil)
  if err != nil {
    return err
  }
  req.Header.Set("Accept", "text/event-stream")

  resp, err := sseClient.Do(req)
  if err != nil {
    return err
  }
  defer resp.Body.Close()

  if resp.StatusCode/100 != 2 {
    body, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
    return fmt.Errorf("status=%d body=%s", resp.StatusCode, strings.TrimSpace(string(body)))
  }

  // Keepalive goroutine: send SSE comment every 20s so zeroclaw and any
  // intermediary (ingress, load-balancer) don't treat a quiet stream as dead.
  keepaliveTicker := time.NewTicker(20 * time.Second)
  defer keepaliveTicker.Stop()
  go func() {
    for {
      select {
      case <-ctx.Done():
        return
      case <-keepaliveTicker.C:
        sw.WriteKeepalive()
      }
    }
  }()

  scanner := bufio.NewScanner(resp.Body)
  buf := make([]byte, 0, 64*1024)
  scanner.Buffer(buf, 1024*1024)
  var dataBuf strings.Builder
  for scanner.Scan() {
    if ctx.Err() != nil {
      return ctx.Err()
    }
    line := scanner.Text()

    // Buffer data lines; enrich and flush on blank line (end of event).
    if after, ok := strings.CutPrefix(line, "data:"); ok {
      dataBuf.WriteString(after)
      continue
    }
    if line == "" && dataBuf.Len() > 0 {
      raw := json.RawMessage(strings.TrimSpace(dataBuf.String()))
      dataBuf.Reset()
      enriched := enrichAttachments(ctx, account, raw)
      sw.WriteEvent(enriched)
      continue
    }

    // Pass through comments and other lines as-is.
    flush := line == "" || strings.HasPrefix(line, ":")
    sw.WriteRaw(append([]byte(line), '\n'), flush)
  }
  if err := scanner.Err(); err != nil {
    return err
  }
  return nil
}

func streamEventsOverWebsocket(ctx context.Context, account string, sw *sseWriter) error {
  wsBase := strings.Replace(strings.Replace(bridgeURL, "https://", "wss://", 1), "http://", "ws://", 1)
  wsURL := fmt.Sprintf("%s/v1/receive/%s", wsBase, url.PathEscape(account))
  log.Printf("events websocket connect account=%s url=%s", account, wsURL)

  conn, resp, err := websocket.DefaultDialer.DialContext(ctx, wsURL, nil)
  if err != nil {
    if resp != nil {
      body, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
      return fmt.Errorf("ws handshake status=%d body=%s err=%w", resp.StatusCode, strings.TrimSpace(string(body)), err)
    }
    return err
  }
  defer conn.Close()

  for {
    if ctx.Err() != nil {
      return ctx.Err()
    }

    mt, msg, err := conn.ReadMessage()
    if err != nil {
      return err
    }
    if mt != websocket.TextMessage && mt != websocket.BinaryMessage {
      continue
    }

    enriched := enrichAttachments(ctx, account, json.RawMessage(msg))
    payload, ok := toSSEPayload(enriched)
    if !ok {
      continue
    }
    sw.WriteEvent(payload)
  }
}

func streamEventsByPolling(ctx context.Context, account string, sw *sseWriter) error {
  ticker := time.NewTicker(15 * time.Second)
  defer ticker.Stop()

  const maxConsecutiveFailures = 5
  consecutiveFailures := 0

  for {
    select {
    case <-ctx.Done():
      return ctx.Err()
    case <-ticker.C:
      sw.WriteKeepalive()
    default:
    }

    path := fmt.Sprintf("/v1/receive/%s?timeout=%s&ignore_attachments=false&ignore_stories=true", url.PathEscape(account), url.QueryEscape(pollTimeout))
    reqCtx, cancel := context.WithTimeout(ctx, 40*time.Second)
    resp, body, err := bridgeRequest(reqCtx, http.MethodGet, path, nil, map[string]string{"Accept": "application/json"})
    cancel()
    if err != nil {
      consecutiveFailures++
      log.Printf("events polling request error account=%s err=%v (consecutive=%d)", account, err, consecutiveFailures)
      if consecutiveFailures >= maxConsecutiveFailures {
        return fmt.Errorf("polling failed %d consecutive times: %w", consecutiveFailures, err)
      }
      time.Sleep(2 * time.Second)
      continue
    }
    if resp.StatusCode/100 != 2 {
      consecutiveFailures++
      log.Printf("events polling bad status account=%s status=%d body=%s (consecutive=%d)", account, resp.StatusCode, strings.TrimSpace(string(body)), consecutiveFailures)
      if consecutiveFailures >= maxConsecutiveFailures {
        return fmt.Errorf("polling got status %d for %d consecutive requests", resp.StatusCode, consecutiveFailures)
      }
      time.Sleep(2 * time.Second)
      continue
    }

    consecutiveFailures = 0
    trimmed := bytes.TrimSpace(body)
    if len(trimmed) == 0 || bytes.Equal(trimmed, []byte("[]")) {
      continue
    }

    var arr []json.RawMessage
    if err := json.Unmarshal(trimmed, &arr); err != nil {
      arr = []json.RawMessage{trimmed}
    }

    for _, item := range arr {
      enriched := enrichAttachments(ctx, account, item)
      payload, ok := toSSEPayload(enriched)
      if !ok {
        continue
      }
      sw.WriteEvent(payload)
    }
  }
}

func handleRPC(w http.ResponseWriter, r *http.Request) {
  if r.Method != http.MethodPost {
    http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
    return
  }

  var req rpcRequest
  if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
    writeRPC(w, http.StatusBadRequest, rpcResponse{JSONRPC: "2.0", Error: &rpcError{Code: -32700, Message: "parse error"}, ID: nil})
    return
  }

  switch req.Method {
  case "send":
    var p sendParams
    if err := json.Unmarshal(req.Params, &p); err != nil {
      writeRPC(w, http.StatusBadRequest, rpcResponse{JSONRPC: "2.0", Error: &rpcError{Code: -32602, Message: "invalid params"}, ID: req.ID})
      return
    }

    if p.Account == "" {
      writeRPC(w, http.StatusBadRequest, rpcResponse{JSONRPC: "2.0", Error: &rpcError{Code: -32602, Message: "missing account"}, ID: req.ID})
      return
    }

    // signal-cli daemon uses JSON-RPC at /api/v1/rpc, not the REST /v2/send endpoint.
    daemonParams := map[string]any{
      "account": p.Account,
      "message": p.Message,
    }
    if p.GroupID != "" {
      daemonParams["groupId"] = p.GroupID
    } else {
      if len(p.Recipient) == 0 {
        writeRPC(w, http.StatusBadRequest, rpcResponse{JSONRPC: "2.0", Error: &rpcError{Code: -32602, Message: "missing recipient"}, ID: req.ID})
        return
      }
      daemonParams["recipient"] = p.Recipient
    }

    paramsJSON, _ := json.Marshal(daemonParams)
    daemonReq := rpcRequest{
      JSONRPC: "2.0",
      Method:  "send",
      Params:  json.RawMessage(paramsJSON),
      ID:      req.ID,
    }
    buf, _ := json.Marshal(daemonReq)
    ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
    resp, body, err := bridgeRequest(ctx, http.MethodPost, "/api/v1/rpc", bytes.NewReader(buf), map[string]string{"Content-Type": "application/json"})
    cancel()
    if err != nil {
      writeRPC(w, http.StatusBadGateway, rpcResponse{JSONRPC: "2.0", Error: &rpcError{Code: -32000, Message: err.Error()}, ID: req.ID})
      return
    }
    if resp.StatusCode/100 != 2 {
      writeRPC(w, http.StatusBadGateway, rpcResponse{JSONRPC: "2.0", Error: &rpcError{Code: -32000, Message: strings.TrimSpace(string(body))}, ID: req.ID})
      return
    }

    var daemonResp rpcResponse
    if err := json.Unmarshal(body, &daemonResp); err == nil && daemonResp.Error != nil {
      writeRPC(w, http.StatusBadGateway, rpcResponse{JSONRPC: "2.0", Error: daemonResp.Error, ID: req.ID})
      return
    }
    // Forward daemon result (includes sent-message timestamp for receipt correlation);
    // fall back to {"ok": true} if the response couldn't be parsed.
    result := daemonResp.Result
    if result == nil {
      result = map[string]bool{"ok": true}
    }
    writeRPC(w, http.StatusOK, rpcResponse{JSONRPC: "2.0", Result: result, ID: req.ID})

  case "sendTyping":
    // signal-cli-rest-api has no dedicated typing endpoint; keep this as a successful no-op.
    w.WriteHeader(http.StatusCreated)

  default:
    writeRPC(w, http.StatusBadRequest, rpcResponse{JSONRPC: "2.0", Error: &rpcError{Code: -32601, Message: "method not found"}, ID: req.ID})
  }
}

// v2SendRequest is the REST v2 /v2/send request body format.
type v2SendRequest struct {
	Message       string   `json:"message"`
	Number        string   `json:"number"`
	Recipients    []string `json:"recipients"`
	EditTimestamp *uint64  `json:"edit_timestamp,omitempty"`
}

// handleV2Send translates REST v2 /v2/send requests into JSON-RPC "send"
// calls to the signal-cli daemon, supporting message editing via editTimestamp.
func handleV2Send(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	var req v2SendRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "invalid request body", http.StatusBadRequest)
		return
	}

	if req.Number == "" || req.Message == "" || len(req.Recipients) == 0 {
		http.Error(w, "missing required fields: number, message, recipients", http.StatusBadRequest)
		return
	}

	// Build JSON-RPC params for signal-cli daemon's "send" method.
	params := map[string]any{
		"account":   req.Number,
		"message":   req.Message,
		"recipient": req.Recipients,
	}
	if req.EditTimestamp != nil {
		params["editTimestamp"] = *req.EditTimestamp
	}

	paramsJSON, _ := json.Marshal(params)
	rpcReq := rpcRequest{
		JSONRPC: "2.0",
		Method:  "send",
		Params:  json.RawMessage(paramsJSON),
		ID:      "v2send-" + fmt.Sprintf("%d", time.Now().UnixNano()),
	}
	buf, _ := json.Marshal(rpcReq)

	ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
	defer cancel()

	resp, body, err := bridgeRequest(ctx, http.MethodPost, "/api/v1/rpc",
		bytes.NewReader(buf), map[string]string{"Content-Type": "application/json"})
	if err != nil {
		log.Printf("v2/send rpc bridge error: %v", err)
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	if resp.StatusCode/100 != 2 {
		log.Printf("v2/send rpc bridge status=%d body=%s", resp.StatusCode, strings.TrimSpace(string(body)))
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusBadGateway)
		_, _ = w.Write(body)
		return
	}

	// Parse JSON-RPC response and extract result.timestamp for the v2 format.
	var rpcResp rpcResponse
	if err := json.Unmarshal(body, &rpcResp); err != nil {
		log.Printf("v2/send rpc response unmarshal error: %v", err)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"timestamp":""}`))
		return
	}
	if rpcResp.Error != nil {
		log.Printf("v2/send rpc error: code=%d msg=%s", rpcResp.Error.Code, rpcResp.Error.Message)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusBadGateway)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": rpcResp.Error.Message})
		return
	}

	// Extract timestamp from result — signal-cli returns {"timestamp": 12345} as number.
	var tsStr string
	if resultMap, ok := rpcResp.Result.(map[string]any); ok {
		if ts, ok := resultMap["timestamp"]; ok {
			switch v := ts.(type) {
			case float64:
				tsStr = fmt.Sprintf("%.0f", v)
			case string:
				tsStr = v
			case json.Number:
				tsStr = v.String()
			}
		}
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]string{"timestamp": tsStr})
}

func main() {
  mux := http.NewServeMux()
  mux.HandleFunc("/api/v1/check", handleCheck)
  mux.HandleFunc("/api/v1/events", handleEvents)
  mux.HandleFunc("/api/v1/rpc", handleRPC)
  mux.HandleFunc("/v2/send", handleV2Send)

  server := &http.Server{
    Addr:              listenAddr,
    Handler:           mux,
    ReadHeaderTimeout: 10 * time.Second,
  }

  log.Printf("signal-api-adapter listening on %s (bridge=%s)", listenAddr, bridgeURL)
  if err := server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
    log.Fatal(err)
  }
}

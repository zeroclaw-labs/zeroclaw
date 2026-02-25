// __SKILL_NAME__ — ZeroClaw Skill (Go / WASI)
//
// Counts words, lines, and characters in text.
// Protocol: read JSON from stdin, write JSON result to stdout.
// Build:    tinygo build -o tool.wasm -target wasi .
// Test:     zeroclaw skill test . --args '{"text":"hello world"}'

package main

import (
	"encoding/json"
	"fmt"
	"io"
	"os"
	"strings"
)

type Args struct {
	Text string `json:"text"`
}

type CountResult struct {
	Words      int `json:"words"`
	Lines      int `json:"lines"`
	Characters int `json:"characters"`
}

type ToolResult struct {
	Success bool         `json:"success"`
	Output  string       `json:"output"`
	Error   *string      `json:"error,omitempty"`
	Data    *CountResult `json:"data,omitempty"`
}

func main() {
	data, err := io.ReadAll(os.Stdin)
	if err != nil {
		writeError(fmt.Sprintf("failed to read stdin: %v", err))
		return
	}

	var args Args
	if err := json.Unmarshal(data, &args); err != nil {
		writeError(fmt.Sprintf("invalid input JSON: %v — expected {\"text\":\"...\"}", err))
		return
	}

	lines := 0
	if args.Text != "" {
		lines = strings.Count(args.Text, "\n") + 1
	}
	counts := CountResult{
		Words:      len(strings.Fields(args.Text)),
		Lines:      lines,
		Characters: len([]rune(args.Text)),
	}

	result := ToolResult{
		Success: true,
		Output:  fmt.Sprintf("%d words, %d lines, %d characters", counts.Words, counts.Lines, counts.Characters),
		Data:    &counts,
	}

	out, _ := json.Marshal(result)
	os.Stdout.Write(out)
}

func writeError(msg string) {
	result := ToolResult{Success: false, Error: &msg}
	out, _ := json.Marshal(result)
	os.Stdout.Write(out)
}

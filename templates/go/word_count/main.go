// __SKILL_NAME__ — ZeroClaw Skill (Go / WASI)
//
// Counts words, lines, and characters in text.
// Protocol: read JSON from stdin, write JSON result to stdout.
// Build:    tinygo build -target=wasip1 -o tool.wasm .
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
		Output: fmt.Sprintf("%d %s, %d %s, %d %s",
			counts.Words, plural(counts.Words, "word", "words"),
			counts.Lines, plural(counts.Lines, "line", "lines"),
			counts.Characters, plural(counts.Characters, "character", "characters"),
		),
		Data: &counts,
	}

	out, err := json.Marshal(result)
	if err != nil {
		fmt.Fprintln(os.Stderr, "json marshal error:", err)
		os.Exit(1)
	}
	os.Stdout.Write(out)
}

func plural(n int, singular, pluralForm string) string {
	if n == 1 {
		return singular
	}
	return pluralForm
}

func writeError(msg string) {
	result := ToolResult{Success: false, Error: &msg}
	out, err := json.Marshal(result)
	if err != nil {
		fmt.Fprintln(os.Stderr, "json marshal error:", err)
		os.Exit(1)
	}
	os.Stdout.Write(out)
}

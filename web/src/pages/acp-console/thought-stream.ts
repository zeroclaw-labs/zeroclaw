export class ThoughtChunkBuffer {
  private content = '';

  append(chunk: string): void {
    this.content += chunk;
  }

  take(): string | null {
    const content = this.content;
    this.content = '';
    return content.trim() ? content : null;
  }

  clear(): void {
    this.content = '';
  }
}

/** Orders polling responses against newer polls and out-of-band writes. */
export class LatestOverlayWriteGate {
  private revision = 0;

  beginRequest(): number {
    this.revision += 1;
    return this.revision;
  }

  supersedePendingRequests(): void {
    this.revision += 1;
  }

  isCurrent(requestRevision: number): boolean {
    return requestRevision === this.revision;
  }
}

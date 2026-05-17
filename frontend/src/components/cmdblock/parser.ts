/**
 * OSC 133 shell-integration parser (BridgeSpace gap #20 — command blocks).
 *
 * The protocol (originated by FinalTerm, adopted by iTerm2/VTE/Wezterm/Warp)
 * marks command boundaries with these escape sequences:
 *
 *     ESC ] 133 ; A ST    prompt start
 *     ESC ] 133 ; B ST    prompt end (command starts being typed)
 *     ESC ] 133 ; C ST    command executed (output starts)
 *     ESC ] 133 ; D ; <code> ST  command exited with status <code>
 *
 * `ST` is either `ESC \` (`\x1b\x5c`) or `BEL` (`\x07`).
 *
 * Users of this parser feed bytes via `feed(chunk)` and read off completed
 * blocks via `take()`. Strings inside OSC envelopes are NOT echoed back into
 * the regular stdout stream — xterm gets the rest verbatim, so the visible
 * output is unchanged.
 */

export interface CommandBlock {
  /** Stable monotonically-increasing id assigned by the parser. */
  id: number;
  /** Bytes that arrived between `B` and `C` (best-effort command text). */
  command: string;
  /** Number of stdout bytes that arrived in the C → D window. */
  outputBytes: number;
  exitCode?: number;
  startedAt: number;
  endedAt?: number;
}

const ESC = 0x1b;
const ST_BS = 0x5c; // backslash, second half of ST
const BEL = 0x07;
const OSC = 0x5d; // ']'

type Phase = "idle" | "prompt" | "command" | "running";

export class CommandBlockParser {
  private phase: Phase = "idle";
  private nextId = 1;
  private collected: CommandBlock[] = [];
  private currentBlock: CommandBlock | null = null;
  private commandBuf = "";
  private decoder = new TextDecoder("utf-8", { fatal: false });

  /**
   * Feed a chunk of bytes from the PTY. Returns the bytes that should be
   * forwarded to xterm — OSC 133 control sequences are stripped out.
   */
  feed(chunk: Uint8Array): Uint8Array {
    const out: number[] = [];
    let i = 0;
    while (i < chunk.length) {
      const byte = chunk[i];
      // Look for the start of any OSC sequence: ESC ']' '1' '3' '3' ';' …
      if (
        byte === ESC &&
        chunk[i + 1] === OSC &&
        chunk[i + 2] === 0x31 && // '1'
        chunk[i + 3] === 0x33 && // '3'
        chunk[i + 4] === 0x33 && // '3'
        chunk[i + 5] === 0x3b // ';'
      ) {
        // Find the terminator: BEL or ESC '\'.
        let j = i + 6;
        let term = -1;
        while (j < chunk.length) {
          if (chunk[j] === BEL) {
            term = j;
            break;
          }
          if (chunk[j] === ESC && chunk[j + 1] === ST_BS) {
            term = j + 1;
            break;
          }
          j++;
        }
        if (term < 0) {
          // Incomplete OSC at end of buffer — pass remainder through, the
          // caller will re-feed. We deliberately don't attempt to buffer
          // partial sequences across feed() calls; OSC 133 sequences are
          // always small (<32 bytes) so this rarely splits in practice.
          for (let k = i; k < chunk.length; k++) out.push(chunk[k]);
          break;
        }
        const body = chunk.subarray(i + 6, term - (chunk[term] === BEL ? 0 : 1));
        this.handleBody(body);
        i = term + 1;
        continue;
      }
      // Outside an OSC envelope.
      if (this.phase === "running" && this.currentBlock) {
        this.currentBlock.outputBytes++;
      } else if (this.phase === "command") {
        this.commandBuf += String.fromCharCode(byte);
      }
      out.push(byte);
      i++;
    }
    return Uint8Array.from(out);
  }

  /** Snapshot and clear the queue of completed/updated blocks. */
  take(): CommandBlock[] {
    const v = this.collected;
    this.collected = [];
    return v;
  }

  /** Currently in-flight block, if any. */
  current(): CommandBlock | null {
    return this.currentBlock;
  }

  private handleBody(body: Uint8Array): void {
    const text = this.decoder.decode(body);
    // Body is e.g. "A", "B", "C", "D;0".
    const parts = text.split(";");
    const tag = parts[0];
    switch (tag) {
      case "A":
        this.phase = "prompt";
        break;
      case "B":
        this.phase = "command";
        this.commandBuf = "";
        break;
      case "C": {
        this.phase = "running";
        const block: CommandBlock = {
          id: this.nextId++,
          command: this.commandBuf.trim(),
          outputBytes: 0,
          startedAt: Date.now(),
        };
        this.currentBlock = block;
        this.collected.push(block);
        this.commandBuf = "";
        break;
      }
      case "D": {
        const code = parts[1] !== undefined ? Number.parseInt(parts[1], 10) : NaN;
        if (this.currentBlock) {
          this.currentBlock.exitCode = Number.isFinite(code) ? code : undefined;
          this.currentBlock.endedAt = Date.now();
          this.collected.push(this.currentBlock);
          this.currentBlock = null;
        }
        this.phase = "idle";
        break;
      }
      default:
        // Unknown subcommand — ignore so future protocol extensions don't
        // crash the parser.
        break;
    }
  }
}

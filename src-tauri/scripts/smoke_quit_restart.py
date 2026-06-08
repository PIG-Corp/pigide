#!/usr/bin/env python3
"""End-to-end test that an agent survives client disconnect.

Run as:  python3 smoke_quit_restart.py
"""
import socket, json, base64, time, subprocess, os, sys

SOCK = "/tmp/pq.sock"
LOG_DIR = "/tmp/pq-logs"
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
AGENTD = os.path.join(SCRIPT_DIR, "../../target/debug/pigide-agentd")


def conn():
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(SOCK)
    s.settimeout(2.0)
    return s


def call(s, frame, expect_id):
    s.sendall((json.dumps(frame) + "\n").encode())
    buf = b""
    while True:
        c = s.recv(1)
        if not c:
            return None
        buf += c
        if buf.endswith(b"\n"):
            f = json.loads(buf.decode())
            if "id" in f and f["id"] == expect_id:
                return f
            buf = b""


def main():
    # Start broker (debug build).
    env = {**os.environ,
           "PIGIDE_AGENTD_SOCKET": SOCK,
           "PIGIDE_AGENTD_LOG_DIR": LOG_DIR,
           "PIGIDE_AGENTD_LOG": "info"}
    proc = subprocess.Popen([AGENTD], env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    broker_pid = proc.pid
    print(f"broker pid: {broker_pid}")

    # Wait for socket and connection.
    for _ in range(100):
        if os.path.exists(SOCK):
            try:
                test_sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                test_sock.settimeout(0.5)
                test_sock.connect(SOCK)
                test_sock.close()
                break
            except ConnectionRefusedError:
                pass
        time.sleep(0.1)
    else:
        raise RuntimeError("broker did not bind socket or accept connections")

    # === Launch 1: spawn agent + write some history ===
    c = conn()
    h = call(c, {"id": 1, "op": "hello", "client_version": 1}, 1)
    print(f"hello: broker_pid={h['hello']['broker_pid']}")

    sp = call(c, {"id": 2, "op": "spawn", "workspace_id": "ws", "agent_type": "t",
                  "cwd": "/tmp", "bin_path": "/bin/cat",
                  "argv": [], "env": [], "reuse_id": None}, 2)
    agent_id = sp["agent"]["id"]
    print(f"spawned: {agent_id}")

    call(c, {"id": 3, "op": "write", "agent_id": agent_id,
             "data_b64": base64.b64encode(b"L1\n").decode()}, 3)
    time.sleep(0.2)

    # Verify the cat process is a child of the broker.
    cat_pids = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/status") as f:
                d = dict(l.split(":\t", 1) for l in f if "\t" in l)
            if d.get("Name", "").strip() == "cat" and \
               int(d.get("PPid", "0").strip()) == broker_pid:
                cat_pids.append(int(entry))
        except (FileNotFoundError, PermissionError):
            pass
    print(f"cat pids under broker: {cat_pids}")
    assert len(cat_pids) == 1, f"expected 1 child, got {cat_pids}"

    # === Disconnect client (imitate Cmd+Q PigIDE) ===
    c.close()
    time.sleep(0.3)

    cat_pids_after = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/status") as f:
                d = dict(l.split(":\t", 1) for l in f if "\t" in l)
            if d.get("Name", "").strip() == "cat" and \
               int(d.get("PPid", "0").strip()) == broker_pid:
                cat_pids_after.append(int(entry))
        except (FileNotFoundError, PermissionError):
            pass
    print(f"cat pids after disconnect: {cat_pids_after}")
    assert cat_pids_after == cat_pids, "AGENT DIED on client disconnect"
    print(f"OK: agent survived (pid {cat_pids[0]} stable)")

    # === Launch 2: fresh client, expect to see same agent ===
    c2 = conn()
    h2 = call(c2, {"id": 1, "op": "hello", "client_version": 1}, 1)
    assert h2["hello"]["broker_pid"] == broker_pid, \
        f"broker pid changed: {h2['hello']['broker_pid']} vs {broker_pid}"

    ls = call(c2, {"id": 2, "op": "list_all"}, 2)
    listed = [a["id"] for a in ls["agents"]]
    print(f"list_all from new client: {listed}")
    assert agent_id in listed, f"agent missing from new client view"

    # Write through the new client; both messages should be in the log.
    call(c2, {"id": 3, "op": "write", "agent_id": agent_id,
              "data_b64": base64.b64encode(b"L2\n").decode()}, 3)
    time.sleep(0.2)

    lt = call(c2, {"id": 4, "op": "log_tail",
                   "agent_id": agent_id, "max_bytes": 4096}, 4)
    log = base64.b64decode(lt["data_b64"]).decode(errors="replace")
    print(f"log content: {log!r}")
    assert "L1" in log and "L2" in log, "scrollback broken"
    print("OK: scrollback preserved across disconnect+reconnect")

    # Cleanup
    call(c2, {"id": 5, "op": "kill", "agent_id": agent_id}, 5)
    c2.close()

    # Stop broker
    proc.terminate()
    proc.wait(timeout=2)
    print()
    print("=== ALL CHECKS PASSED ===")


if __name__ == "__main__":
    sys.exit(main() or 0)

"""Drive `infinity acp` (stock binary) with one prompt and print what happens."""
import json, os, subprocess, sys, threading, time, queue

env = dict(os.environ)
p = subprocess.Popen([sys.argv[1], "acp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=open(sys.argv[3], "w"), env=env, text=True, bufsize=1)
q = queue.Queue()
threading.Thread(target=lambda: [q.put(l) for l in p.stdout], daemon=True).start()
nid = 0
def send(method, params, notify=False):
    global nid
    msg = {"jsonrpc": "2.0", "method": method, "params": params}
    if not notify:
        nid += 1; msg["id"] = nid
    p.stdin.write(json.dumps(msg) + "\n"); p.stdin.flush()
    return nid
def respond(i, result):
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": i, "result": result}) + "\n"); p.stdin.flush()

def wait_result(i, timeout=60):
    end = time.time() + timeout
    while time.time() < end:
        try: line = q.get(timeout=1)
        except queue.Empty: continue
        m = json.loads(line)
        if m.get("id") == i and "method" not in m: return m
        handle(m)
    raise SystemExit(f"timeout waiting for response {i}")

text = []
def handle(m):
    if m.get("method") == "session/request_permission":
        opts = m["params"].get("options", [])
        pick = next((o["optionId"] for o in opts if "allow" in o.get("kind", "")), opts[0]["optionId"] if opts else None)
        print("PERMISSION asked:", json.dumps(m["params"].get("toolCall", {}))[:160], "->", pick)
        respond(m["id"], {"outcome": {"outcome": "selected", "optionId": pick}})
    elif m.get("method") == "session/update":
        u = m["params"]["update"]; k = u.get("sessionUpdate")
        if k == "agent_message_chunk": text.append(u["content"].get("text", ""))
        elif k in ("tool_call", "tool_call_update"):
            st = u.get("status"); title = u.get("title") or ""
            if k == "tool_call" or st in ("completed", "failed"):
                print(f"[{k}] {title} {st or ''} {json.dumps(u.get('rawInput', ''))[:120]}")

r = wait_result(send("initialize", {"protocolVersion": 1, "clientCapabilities": {}}))
sid = wait_result(send("session/new", {"cwd": sys.argv[2], "mcpServers": []}))["result"]["sessionId"]
print("session", sid)
pid = send("session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": sys.stdin.read()}]})
res = wait_result(pid, timeout=int(os.environ.get("TIMEOUT", "240")))
print("STOP:", json.dumps(res.get("result") or res.get("error"))[:200])
# RAP tools answer later; the agent wakes on each callback, so keep listening.
end = time.time() + int(os.environ.get("TIMEOUT", "240"))
while time.time() < end and "GOT EVENT" not in "".join(text):
    try: line = q.get(timeout=1)
    except queue.Empty: continue
    handle(json.loads(line))
print("MODEL SAID:\n" + "".join(text))
p.terminate()

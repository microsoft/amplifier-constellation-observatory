"""Disposable ACP peer: records actual prompt/effect counts and asynchronous input."""
import json
import os
import select
import sys
import time

mode, journal = sys.argv[1:]
session = "saved-fixture"
active = None
updates = []
permission = False


def record(kind, **data):
    with open(journal, "a", encoding="utf-8") as out:
        out.write(json.dumps(dict(kind=kind, **data)) + "\n")


def send(**data):
    print(json.dumps(dict(jsonrpc="2.0", **data)), flush=True)


def finish():
    global active, updates
    record("effect", text=active["text"], updates=updates)
    send(method="session/update", params={"sessionId": session, "update": {
        "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Finished: " + active["text"] + " / " + " / ".join(updates)}}})
    send(id=active["id"], result={"stopReason": "end_turn"})
    active, updates = None, []


while True:
    if active and not permission and time.monotonic() >= active["due"]:
        finish()
    if not select.select([sys.stdin], [], [], 0.02)[0]:
        continue
    line = sys.stdin.readline()
    if not line:
        break
    r = json.loads(line)
    method, rid = r.get("method"), r.get("id")
    if method == "initialize":
        result = {"protocolVersion": 1, "agentCapabilities": {"loadSession": True}}
        if mode != "generic":
            result["_meta"] = {"steering": {"supported": True, "version": 1, "idleBehavior": ["promptRequired"]}}
        if mode in ("recover", "bad-recovery"):
            result["_meta"]["steering"].update(receiptsMethod="_session/input_receipts", acknowledgeMethod="_session/acknowledge_inputs")
        send(id=rid, result=result)
    elif method in ("session/new", "session/load"):
        record(method)
        send(id=rid, result={"sessionId": session})
    elif method == "session/prompt":
        assert active is None, "concurrent prompts"
        text = r["params"]["prompt"][0]["text"]
        record("prompt", text=text)
        active = dict(id=rid, text=text, due=time.monotonic() + 0.7)
        if text == "approval":
            permission = True
            send(id="permission", method="session/request_permission", params={"sessionId": session, "toolCall": {"toolCallId": "write", "title": "Write once"}, "options": [{"optionId": "once", "name": "Allow once", "kind": "allow_once"}]})
        if text == "partial":
            # Tick cancellation must not discard a partially read JSON frame.
            frame=json.dumps(dict(jsonrpc="2.0",method="session/update",params={"sessionId":session,"update":{"sessionUpdate":"tool_call","toolCallId":"big","title":"x"*20000}}))+"\n"
            for part in [frame[:8000],frame[8000:16000],frame[16000:]]:
                sys.stdout.write(part);sys.stdout.flush();time.sleep(0.15)
    elif method == "_session/steering":
        assert mode != "generic"
        assert r["params"]["sessionId"] == session
        meta = r["params"]["_meta"]["steering"]
        assert meta["idleBehavior"] == "promptRequired"
        text = r["params"]["prompt"][0]["text"]
        record("steer", text=text, input=meta["inputId"])
        if mode in ("drop", "recover", "bad-recovery"):
            os._exit(0)
        if mode == "idle" or active is None:
            if active: finish()
            send(id=rid, result={"outcome": "promptRequired"})
        elif mode == "failed":
            send(id=rid, result={"outcome": "failed"})
        elif mode == "malformed":
            send(id=rid, result={"outcome": "invented"})
        else:
            updates.append(text)
            # Incorporation can precede the request acknowledgment.
            send(method="session/update", params={"sessionId": session, "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": ""}, "_meta": {"io.github.bkrabach.agent-runtime": {"inputReceipt": {"inputId": meta["inputId"], "state": "incorporated"}}}}})
            send(id=rid, result={"outcome": "injected"})
            send(method="session/update", params={"sessionId": "another-room", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "PRIVATE OTHER ROOM"}, "_meta": {"io.github.bkrabach.agent-runtime": {"inputReceipt": {"inputId": meta["inputId"], "state": "unknown"}}}}})
    elif method in ("_session/input_receipts", "_session/acknowledge_inputs"):
        with open(journal, encoding="utf-8") as src:
            ids = [e["input"] for e in map(json.loads, src) if e["kind"] == "steer"]
        if method == "_session/acknowledge_inputs":
            assert r["params"]["inputIds"] == ids
            assert r["params"]["disposition"] == "proceed_without_replay"
            record("acknowledge", ids=ids)
        send(id=rid,result={"inputReceipts":[{"inputId":i,"state":"incorporated","recorded":False,"reconciled":method=="_session/acknowledge_inputs" and mode!="bad-recovery"} for i in ids]})
    elif method == "session/cancel":
        record("cancel")
        permission = False
        if active:
            send(id=active["id"], result={"stopReason": "cancelled"})
            active = None
    elif rid == "permission":
        assert r["result"]["outcome"]["optionId"] == "once"
        record("permission")
        permission = False
        active["due"] = time.monotonic() + 0.1
    else:
        send(id=rid, result={})

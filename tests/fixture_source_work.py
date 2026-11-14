"""Deterministic ACP profile peer; no model, tools, network or real work."""
import json
import pathlib
import sys
import time

mode, log_path = sys.argv[1:]
log = pathlib.Path(log_path)
namespace = "com.microsoft.amplifier.source-delivery"


def receive():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    message = json.loads(line)
    with log.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(message) + "\n")
    return message


def send(message):
    print(json.dumps(message), flush=True)


def prompts():
    return [row for row in map(json.loads, log.read_text().splitlines())
            if row.get("method") == "session/prompt"]


def reject_interactions(stage, request_id):
    question = stage + "-question"
    send({"jsonrpc": "2.0", "id": question, "method": "elicitation/create",
          "params": {"requestId": request_id, "mode": "form", "message": "Not authorized by inspection",
                     "requestedSchema": {"type": "object", "properties": {"answer": {"type": "string"}}, "required": ["answer"]}}})
    response = receive()
    assert response["id"] == question and response["error"]["code"] == -32602
    permission = stage + "-permission"
    send({"jsonrpc": "2.0", "id": permission, "method": "session/request_permission",
          "params": {"sessionId": "saved-source-session", "toolCall": {"toolCallId": "not-work", "title": "Not authorized by inspection"},
                     "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"}]}})
    response = receive()
    assert response["id"] == permission
    assert response["result"]["outcome"]["outcome"] == "cancelled"
    log.with_name(stage + "-callbacks-refused").write_text("refused", encoding="utf-8")


while True:
    message = receive()
    method, params = message.get("method"), message.get("params", {})
    if "id" not in message:
        continue
    if method == "initialize":
        result = {"protocolVersion": 1, "agentInfo": {"name": "source-fixture", "version": "1"}, "agentCapabilities": {"loadSession": True}}
        if mode == "restore-auth":
            result["authMethods"] = [{"id": "fixture-login", "name": "Fixture login", "type": "agent"}]
        if mode.startswith("restore-") and prompts():
            assert params["clientCapabilities"] == {}, "Inspection advertised interactive authority"
            if mode == "restore-interactions":
                reject_interactions("initialize", message["id"])
        if mode != "unsupported":
            result["_meta"] = {"io.github.bkrabach.agent-instance": {"inputContinuity": {"sourceDelivery": {"version": 1, "metadata": namespace, "receiptsMethod": "_amplifier/source_receipts", "ordinaryPrompts": True, "promptRetry": "inspectOnly", "workReferences": True}}}}
    elif method == "session/new":
        result = {"sessionId": "saved-source-session"}
    elif method == "session/load":
        assert params["sessionId"] == "saved-source-session"
        if mode == "restore-interactions":
            reject_interactions("restore", message["id"])
        if mode == "restore-auth":
            send({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32000, "message": "Fixture authentication required"}})
            continue
        result = {}
    elif method == "session/prompt":
        if mode in ("lost", "malformed", "retained", "empty") or mode.startswith("restore-"):
            sys.exit(0)
        result = {"stopReason": "end_turn"}
    elif method == "_amplifier/source_receipts":
        if mode == "inspect-barrier":
            log.with_name("inspect-entered").write_text("entered", encoding="utf-8")
            deadline = time.monotonic() + 10
            while not log.with_name("inspect-release").exists():
                assert time.monotonic() < deadline, "Inspection fixture barrier was not released"
                time.sleep(0.01)
        original = prompts()[0]["params"]["_meta"][namespace]
        assert params["sources"] == original["sources"]
        source = params["sources"][0]
        receipt = {"sourceId": source["id"], "revision": source["revision"], "occurredAt": source["occurredAt"], "inputId": original["inputId"], "state": "unknown", "incorporated": False, "response": None, "observedAt": source["occurredAt"], "completion": "not_established", "workCoverage": "retained_session_records", "work": [
            {"id": "11111111-1111-4111-8111-111111111111", "ownerAgentId": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "revision": 2, "state": "accepted", "inputId": original["inputId"]},
            {"id": "22222222-2222-4222-8222-222222222222", "ownerAgentId": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "revision": 1, "state": "proposed", "inputId": original["inputId"]}
        ]}
        if mode == "malformed":
            receipt["sourceId"] = "different-source"
        if mode == "retained":
            receipt["inputId"] = None
            receipt["observedAt"] = None
        result = {"version": 1, "receipts": [] if mode == "empty" else [receipt], "coverage": {"delivery": "recent_session_records", "work": "retained_session_records", "missing": "unknown"}}
    else:
        raise AssertionError("Unexpected method " + str(method))
    send({"jsonrpc": "2.0", "id": message["id"], "result": result})

"""ACP history fixture: deletion survives its short-lived client connection."""
import json
import pathlib
import sys

removed = pathlib.Path(sys.argv[1])
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    params = request.get("params", {})
    result = {}
    if method == "initialize":
        result = {"protocolVersion": 1, "agentCapabilities": {
            "sessionCapabilities": {"list": {}, "delete": {}}}}
    elif method == "session/list":
        result = {"sessions": [] if removed.exists() else [{
            "sessionId": "history-one", "cwd": params["cwd"],
            "title": "Earlier work", "updatedAt": "2026-01-01T00:00:00Z"}]}
    elif method == "session/delete":
        assert params["sessionId"] == "history-one"
        removed.touch()
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)

"""Assert the runtime sends policy on every create/load/resume request."""
import json
from pathlib import Path
import sys

mode = "auto"
for line in sys.stdin:
    request = json.loads(line)
    with Path("requests.jsonl").open("a") as log:
        log.write(line)
    method, params = request["method"], request.get("params", {})
    if method == "initialize":
        result = {"protocolVersion": 1, "agentCapabilities": {
            "loadSession": sys.argv[1] != "session/resume",
            "sessionCapabilities": {"resume": True},
        }}
    elif method in ("session/new", "session/load", "session/resume"):
        assert method == sys.argv[1], method
        assert params["_meta"]["yoloMode"] is True
        result = {"sessionId": params.get("sessionId", "created"), "modes": {
            "currentModeId": mode, "availableModes": [{"id": "auto"}, {"id": "full-access"}],
        }}
    elif method == "session/set_mode":
        mode = params["modeId"]
        assert mode == "full-access"
        result = {}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)

#!/usr/bin/env python3
"""Minimal ACP stdio agent for runtime turn-streaming tests."""

import json
import os
import sys
import time


fast_enabled = False
current_model = "fake-1"
current_effort = "medium"


def config_options():
    if "--no-fast" in sys.argv:
        return []
    return [
        {
            "id": "fast-mode",
            "type": "boolean",
            "currentValue": fast_enabled,
        }
    ]


def models_payload():
    return {
        "currentModelId": current_model,
        "availableModels": [
            {
                "modelId": "fake-1",
                "name": "Fake One",
                "_meta": {
                    "reasoningEffort": current_effort,
                    "reasoningEfforts": [
                        {"value": "low", "label": "Low"},
                        {"value": "medium", "label": "Medium", "default": True},
                        {"value": "high", "label": "High"},
                    ],
                },
            },
            {
                "modelId": "fake-2",
                "name": "Fake Two",
                "_meta": {
                    "reasoningEffort": "low",
                    "reasoningEfforts": [
                        {"value": "low", "label": "Low"},
                        {"value": "high", "label": "High"},
                    ],
                },
            },
        ],
    }


def session_result(session_id):
    return {
        "sessionId": session_id,
        "configOptions": config_options(),
        "models": models_payload(),
    }


def send(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def prompt_text(params):
    blocks = params.get("prompt") or []
    return "".join(
        block.get("text", "")
        for block in blocks
        if isinstance(block, dict) and block.get("type") == "text"
    )


def handle(msg):
    global fast_enabled, current_model, current_effort
    method = msg.get("method")
    req_id = msg.get("id")
    params = msg.get("params") or {}
    if method == "initialize":
        send(
            {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "protocolVersion": 1,
                    "agentCapabilities": {
                        "promptCapabilities": {},
                        "loadSession": True,
                        "sessionCapabilities": {"resume": True, "load": True},
                    },
                    "agentInfo": {"name": "fake-acp"},
                },
            }
        )
        return
    if method == "session/new":
        send(
            {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": session_result("fake-session"),
            }
        )
        return
    if method in ("session/load", "session/resume"):
        meta = params.get("_meta") or {}
        if meta.get("reasoningEffort"):
            current_effort = str(meta.get("reasoningEffort"))
        send(
            {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": session_result(params.get("sessionId") or "fake-session"),
            }
        )
        return
    if method == "session/set_model":
        model_id = params.get("modelId")
        if model_id:
            current_model = str(model_id)
            send(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": session_result(params.get("sessionId") or "fake-session"),
                }
            )
        else:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": {"code": -32602, "message": "modelId is required"},
                }
            )
        return
    if method == "session/set_config_option":
        if params.get("configId") == "fast-mode" and "--no-fast" not in sys.argv:
            fast_enabled = params.get("value") is True
            send(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {"configOptions": config_options()},
                }
            )
        else:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": {"code": -32602, "message": "unsupported config"},
                }
            )
        return
    if method == "session/prompt":
        sid = params.get("sessionId") or "fake-session"
        text = prompt_text(params)
        if "need-permission" in text:
            perm_id = 900000 + int(req_id)
            send(
                {
                    "jsonrpc": "2.0",
                    "id": perm_id,
                    "method": "session/request_permission",
                    "params": {
                        "sessionId": sid,
                        "toolCall": {"title": "run ls", "kind": "execute"},
                        "options": [
                            {
                                "optionId": "allow-always",
                                "name": "Always",
                                "kind": "allow_always",
                            },
                            {
                                "optionId": "reject",
                                "name": "Reject",
                                "kind": "reject_once",
                            },
                        ],
                    },
                }
            )
            reply_line = sys.stdin.readline()
            reply = json.loads(reply_line) if reply_line else {}
            option = ((reply.get("result") or {}).get("outcome") or {}).get("optionId")
            if option != "allow-always":
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {"stopReason": "cancelled"},
                    }
                )
                return
        if "rpc-error" in text:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": {"code": -32001, "message": "forced prompt failure"},
                }
            )
            return
        if "exit-before-response" in text:
            sys.exit(17)
        if "cancelled-response" in text:
            send({"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "cancelled"}})
            return
        response_text = "fast=true" if text == "check-fast" and fast_enabled else "done"
        send(
            {
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": sid,
                    "update": {
                        "sessionUpdate": "agent_thought_chunk",
                        "content": {"type": "text", "text": "working"},
                    },
                },
            }
        )
        send(
            {
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": sid,
                    "update": {
                        "sessionUpdate": "tool_call",
                        "toolCallId": "call-1",
                        "title": "ls",
                        "kind": "execute",
                        "status": "in_progress",
                        "rawInput": {"command": "ls"},
                    },
                },
            }
        )
        default_delay_ms = "1500" if text in {"hello", "slow-cancel"} else "20"
        time.sleep(int(os.environ.get("FAKE_ACP_PROMPT_DELAY_MS", default_delay_ms)) / 1000.0)
        send(
            {
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": sid,
                    "update": {
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": "call-1",
                        "status": "completed",
                    },
                },
            }
        )
        send(
            {
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": sid,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": response_text},
                    },
                },
            }
        )
        send({"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "end_turn"}})
        return
    if req_id is not None:
        send(
            {
                "jsonrpc": "2.0",
                "id": req_id,
                "error": {"code": -32601, "message": "Method not found"},
            }
        )


def main():
    while True:
        line = sys.stdin.readline()
        if not line:
            break
        line = line.strip()
        if not line:
            continue
        handle(json.loads(line))


if __name__ == "__main__":
    main()

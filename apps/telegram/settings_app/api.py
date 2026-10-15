"""Business rules of the settings App. Every method authorises first, writes the shared state, and audits the change.

Secrets: the bot token goes in only through `set_token`, is verified with Telegram's getMe (fixed API host, no
redirects) before anything is stored, lives in the private token file, and is never returned, logged or put in an
error: callers get the bot's username and the token's last four characters.
"""
from __future__ import annotations

import collections
import re
import time
from typing import Any, Callable

from telegram_app.common import BridgeError, TelegramError
from telegram_app.poc import CHANNEL, PocError, PocService, describe, parse_conversation
from telegram_app.registry import PAIR_TTL
from telegram_app.state import StateStore
from telegram_app.tokenstore import TokenStore, last4, valid_shape

from .auth import Error

ADMIN_ROLES = ("owner", "admin")
HEARTBEAT_STALE = 30.0
LABEL = re.compile(r"^[^\x00-\x1f\x7f]{1,64}$")


def actor_of(principal: dict[str, Any]) -> str:
    return f"{principal['kind']}:{principal['id']}"


class RateLimit:
    """Sliding-window limits kept in memory (this App is a single process)."""
    def __init__(self, clock: Callable[[], float]) -> None:
        self.clock, self.hits = clock, collections.defaultdict(collections.deque)

    def allow(self, key: str, limit: int, window: float) -> bool:
        now, hits = self.clock(), self.hits[key]
        while hits and hits[0] <= now - window:
            hits.popleft()
        if len(hits) >= limit:
            return False
        hits.append(now)
        return True


class Settings:
    def __init__(self, state: StateStore, tokens: TokenStore, cli: Any, bot_factory: Callable[[str], Any], *,
                 admin_agent_ids: frozenset[str] = frozenset(), default_poc: str = "", clock: Callable[[], float] = time.time) -> None:
        self.state, self.tokens, self.cli, self.bot_factory = state, tokens, cli, bot_factory
        self.admin_agent_ids, self.default_poc, self.clock = admin_agent_ids, default_poc, clock
        self.limits = RateLimit(clock)

    # --- access --------------------------------------------------------------------------------

    def authorize(self, principal: dict[str, Any]) -> None:
        """Only owners/admins (humans) and explicitly listed Agents. Hiding controls in the page is not authorisation."""
        if principal["kind"] == "human" and principal.get("role") in ADMIN_ROLES:
            return
        if principal["kind"] == "agent" and principal["id"] in self.admin_agent_ids:
            return
        raise Error(403, "forbidden", "Only workspace owners/admins (and Agents listed by the operator) can change these settings")

    def _account(self) -> str | None:
        return self.state.get_setting("bot_id")

    def _poc(self) -> PocService:
        account = self._account()
        if account is None:
            raise Error(409, "no_bot", "Save a bot token first")
        return PocService(self.state, self.cli, account)

    # --- read ------------------------------------------------------------------------------------

    def status(self, principal: dict[str, Any]) -> dict[str, Any]:
        self.authorize(principal)
        now = self.clock()
        account = self._account()
        settings = self.state.settings_with_age("")
        heartbeat = settings.get("bridge_heartbeat")
        age = None
        try:
            age = now - float(heartbeat[0]) if heartbeat else None
        except ValueError:
            age = None
        bridge_state = settings.get("bridge_state", ("unknown", 0))[0]
        if age is None or age > HEARTBEAT_STALE:
            bridge_state = "offline"
        pairing = self.state.pairing_status(now)
        return {
            "me": {"id": principal["id"], "name": principal.get("name", ""), "kind": principal["kind"], "role": principal.get("role", "")},
            "bot": {"configured": self.tokens.exists(), "username": settings.get("bot_username", ("", 0))[0] or None,
                    "last4": settings.get("token_last4", ("", 0))[0] or None},
            "bridge": {"state": bridge_state, "error": settings.get("bridge_error", ("", 0))[0] or None,
                       "heartbeat_age": None if age is None else round(age, 1)},
            "paused": self.state.get_setting("paused") == "1",
            "pairing": None if pairing is None else {"expires_at": pairing["expires_at"], "created_by": pairing["created_by"], "poc": pairing["poc_agent_id"]},
            "bindings": [] if account is None else [describe(b) | {"updated_at": b.updated_at} for b in self.state.list_bindings(CHANNEL, account)],
            "users": [] if account is None else self.state.list_users(CHANNEL, account),
            "audit": [] if account is None else self.state.audit_list(CHANNEL, account, 50),
            "default_poc": self.default_poc,
            "pair_ttl": PAIR_TTL,
        }

    # --- token -------------------------------------------------------------------------------------

    def set_token(self, principal: dict[str, Any], token: Any) -> dict[str, Any]:
        self.authorize(principal)
        actor = actor_of(principal)
        if not self.limits.allow(f"token:{actor}", 5, 60) or not self.limits.allow("token:all", 20, 600):
            raise Error(429, "rate_limited", "Too many attempts; wait a minute")
        if not isinstance(token, str) or not valid_shape(token.strip()):
            raise Error(400, "invalid_token", "That does not look like a Telegram bot token")
        token = token.strip()
        try:
            me = self.bot_factory(token).get_me()
        except TelegramError as error:
            if error.code == "telegram_request_rejected":
                raise Error(400, "token_rejected", "Telegram did not accept that token") from None
            raise Error(502, "telegram_unavailable", "Could not reach Telegram to check the token") from None
        except BridgeError:
            raise Error(502, "telegram_unavailable", "Could not reach Telegram to check the token") from None
        bot_id, username = str(me.get("id", "")), me.get("username")
        if not bot_id.isdigit() or me.get("is_bot") is not True or not isinstance(username, str) or not re.fullmatch(r"[A-Za-z0-9_]{1,64}", username):
            raise Error(400, "token_rejected", "Telegram did not accept that token")
        existing = self._account()
        if existing is not None and existing != bot_id:
            raise Error(409, "different_bot", "This token belongs to a different bot than the one this bridge was set up with")
        self.tokens.write(token)
        self.state.set_setting("bot_id", bot_id)
        self.state.set_setting("bot_username", username)
        self.state.set_setting("token_last4", last4(token))
        self.state.audit(actor, "settings", "token.set", CHANNEL, bot_id, None, outcome=f"bot:{username} last4:{last4(token)}")
        return {"bot": {"username": username, "last4": last4(token)}}

    def clear_token(self, principal: dict[str, Any]) -> dict[str, Any]:
        self.authorize(principal)
        had = self.tokens.clear()
        self.state.delete_settings("token_last4")
        self.state.invalidate_pairings(self.clock())
        self.state.audit(actor_of(principal), "settings", "token.clear", CHANNEL, self._account() or "-", None, outcome="cleared" if had else "none stored")
        return {"cleared": had}

    # --- pairing -----------------------------------------------------------------------------------

    def create_pairing(self, principal: dict[str, Any], poc: Any) -> dict[str, Any]:
        self.authorize(principal)
        service = self._poc()
        if not self.tokens.exists() and self.state.get_setting("bridge_state") != "running":
            raise Error(409, "no_bot", "Save a bot token first")
        target = poc if isinstance(poc, str) and poc.strip() else self.default_poc
        try:
            agent = service.resolve_agent(target)
        except PocError as error:
            raise Error(400, error.code, error.message) from None
        if not self.limits.allow(f"pair:{actor_of(principal)}", 10, 600):
            raise Error(429, "rate_limited", "Too many codes; wait a few minutes")
        code, expires = self.state.create_pairing(actor_of(principal), agent, now=self.clock())
        self.state.audit(actor_of(principal), "settings", "pairing.create", CHANNEL, self._account() or "-", None, new_target=agent, outcome="ok")
        return {"code": code, "expires_at": expires, "poc": agent}

    def cancel_pairing(self, principal: dict[str, Any]) -> dict[str, Any]:
        self.authorize(principal)
        count = self.state.invalidate_pairings(self.clock())
        self.state.audit(actor_of(principal), "settings", "pairing.cancel", CHANNEL, self._account() or "-", None, outcome=f"invalidated:{count}")
        return {"invalidated": count}

    # --- POCs, users, pause --------------------------------------------------------------------------

    def set_poc(self, principal: dict[str, Any], conversation: Any, to: Any, wake: Any) -> dict[str, Any]:
        self.authorize(principal)
        service = self._poc()
        try:
            key = parse_conversation(conversation)
            if wake is not None and not isinstance(wake, bool):
                raise PocError("invalid_field", "wake_agent must be true or false")
            _, new = service.set_poc(key, to, actor=actor_of(principal), via="settings", op="poc.set", enabled=True, wake=wake)
        except ValueError as error:
            raise Error(400, "invalid_conversation", str(error)) from None
        except PocError as error:
            raise Error(400, error.code, error.message) from None
        return {"binding": describe(new)}

    def toggle_binding(self, principal: dict[str, Any], conversation: Any, enabled: bool) -> dict[str, Any]:
        self.authorize(principal)
        service = self._poc()
        try:
            _, new = service.set_poc(parse_conversation(conversation), None, actor=actor_of(principal), via="settings",
                                     op="poc.enable" if enabled else "poc.disable", enabled=enabled)
        except ValueError as error:
            raise Error(400, "invalid_conversation", str(error)) from None
        except PocError as error:
            raise Error(404 if error.code == "no_binding" else 400, error.code, error.message) from None
        return {"binding": describe(new)}

    def set_user(self, principal: dict[str, Any], user_id: Any, label: Any, enabled: Any) -> dict[str, Any]:
        self.authorize(principal)
        account = self._account()
        if account is None:
            raise Error(409, "no_bot", "Save a bot token first")
        text = str(user_id) if isinstance(user_id, (int, str)) and not isinstance(user_id, bool) else ""
        if not re.fullmatch(r"[0-9]{1,20}", text):
            raise Error(400, "invalid_user", "user_id must be a numeric Telegram user ID")
        label = " ".join(label.split()) if isinstance(label, str) else ""
        if not LABEL.fullmatch(label) or not isinstance(enabled, bool):
            raise Error(400, "invalid_user", "label must be 1-64 printable characters and enabled true or false")
        self.state.set_user(CHANNEL, account, str(int(text)), label, enabled, actor=actor_of(principal), via="settings")
        return {"user": {"user_id": str(int(text)), "label": label, "enabled": enabled}}

    def set_paused(self, principal: dict[str, Any], paused: bool) -> dict[str, Any]:
        self.authorize(principal)
        self.state.set_setting("paused", "1" if paused else "0")
        self.state.audit(actor_of(principal), "settings", "bridge.pause" if paused else "bridge.resume", CHANNEL, self._account() or "-", None, outcome="ok")
        return {"paused": paused}

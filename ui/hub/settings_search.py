"""Secure settings & diagnostics classifier for the Playbox hub.

Turns a human or agent query (e.g. "tailscale", "fix offline alerts", "revoke passkey", "dark mono theme")
into ranked, allowlisted settings targets with intent classification, security tier tags, and actionable
fix hints. Strictly read-only: never executes actions, never interpolates into shell/SQL, and never
returns secrets, tokens, keys, or raw session IDs.
"""
import re

MAX_QUERY = 160
TOKEN_RE = re.compile(r"[a-z0-9][a-z0-9_.-]*")
SUSPICIOUS_RE = re.compile(r"(?:ignore\s+previous|system\s+prompt|<script|javascript:|;\s*rm\b|\$\(|`)", re.I)

# Curated, strictly allowlisted registry of every hub setting and safe diagnostic target.
CATALOG = (
    {
        "id": "overview",
        "page": "overview",
        "group": "overview",
        "title": "Settings home",
        "summary": "Live overview of network, security, notifications, and appearance.",
        "synonyms": "home overview dashboard settings main start summary",
        "intents": ("navigation", "troubleshoot_fix"),
        "security_tier": "read_only",
        "fix_hint": "Start here for live status cards and quick fixes.",
        "api": None,
    },
    {
        "id": "network.mode",
        "page": "network",
        "group": "hub",
        "title": "Network & public access",
        "summary": "Switch between Local only, Tailscale tailnet, or Cloudflare Access tunnel.",
        "synonyms": "network tailscale cloudflare tunnel public remote access expose domain hostname url offline reachability",
        "intents": ("modify_setting", "troubleshoot_fix", "security_audit"),
        "security_tier": "step_up",
        "fix_hint": "Requires passkey step-up. Choose Local, Tailscale, or Cloudflare.",
        "api": {"method": "POST", "path": "/api/settings/network"},
    },
    {
        "id": "hub.devices",
        "page": "devices",
        "group": "hub",
        "title": "Devices & passkeys",
        "summary": "View enrolled passkeys, pair a new phone or laptop, or remove a lost device.",
        "synonyms": "device passkey webauthn touchid faceid pair phone laptop revoke remove sign-in login auth mfa",
        "intents": ("security_audit", "modify_setting", "troubleshoot_fix"),
        "security_tier": "step_up",
        "fix_hint": "Removing a device signs it out immediately and revokes its passkey.",
        "api": {"method": "DELETE", "path": "/api/devices/{id}"},
    },
    {
        "id": "hub.sessions",
        "page": "sessions",
        "group": "hub",
        "title": "Active sessions",
        "summary": "Inspect signed-in browsers, upgrade weak HTTP sessions to passkeys, or sign out.",
        "synonyms": "session browser cookie sign out logout weak http upgrade passkey revoke token",
        "intents": ("security_audit", "troubleshoot_fix", "modify_setting"),
        "security_tier": "step_up",
        "fix_hint": "Upgrade weak sessions to passkeys or revoke unfamiliar browsers.",
        "api": {"method": "DELETE", "path": "/api/sessions/{id}"},
    },
    {
        "id": "safety.approvals",
        "page": "approvals",
        "group": "safety",
        "title": "Capability approvals",
        "summary": "Approve or deny pending capability calls waiting on the room owner.",
        "synonyms": "approval approve deny capability witness hold pause permission agent PLAYBOX_APPROVERS safety guard",
        "intents": ("security_audit", "troubleshoot_fix"),
        "security_tier": "step_up",
        "fix_hint": "Pending calls expire after 15 minutes; approving requires passkey step-up.",
        "api": {"method": "POST", "path": "/api/approvals/{room}/{call}"},
    },
    {
        "id": "notify.push",
        "page": "push",
        "group": "notifications",
        "title": "Push notifications",
        "summary": "Enable or disable web push on this device and send a test notification.",
        "synonyms": "push notification bell alert phone pwa buzz test subscribe unsubscribe",
        "intents": ("modify_setting", "troubleshoot_fix"),
        "security_tier": "safe",
        "fix_hint": "Requires HTTPS (or localhost) and browser notification permission.",
        "api": {"method": "POST", "path": "/api/push/test"},
    },
    {
        "id": "notify.alerts",
        "page": "alerts",
        "group": "notifications",
        "title": "Down & unreachable alerts",
        "summary": "Set how long a room or tunnel may stay unreachable before alerting urgently.",
        "synonyms": "alert down unreachable threshold timeout minutes urgent tunnel offline notify PLAYBOX_DOWN_ALERT_S",
        "intents": ("modify_setting", "troubleshoot_fix"),
        "security_tier": "step_up",
        "fix_hint": "Adjust down-alert threshold (60s to 1h); requires passkey step-up.",
        "api": {"method": "POST", "path": "/api/settings/notify"},
    },
    {
        "id": "notify.mute",
        "page": "mute",
        "group": "notifications",
        "title": "Room notification muting",
        "summary": "Mute or unmute push notifications per room without hiding Attention items.",
        "synonyms": "mute silence quiet noisy room push notifications spam",
        "intents": ("modify_setting",),
        "security_tier": "safe",
        "fix_hint": "Toggle per-room push muting; Attention still shows holds and chain failures.",
        "api": {"method": "PATCH", "path": "/api/rooms/{id}"},
    },
    {
        "id": "appearance.theme",
        "page": "theme",
        "group": "appearance",
        "title": "Theme & color palette",
        "summary": "Choose the visual skin (Mono, Catppuccin, Nord, Solarized, etc.).",
        "synonyms": "theme skin colour color palette mono dark light catppuccin nord solarized look appearance",
        "intents": ("appearance", "modify_setting"),
        "security_tier": "safe",
        "fix_hint": "Stored locally in this browser; switches immediately.",
        "api": None,
    },
    {
        "id": "appearance.accent",
        "page": "accent",
        "group": "appearance",
        "title": "Accent colour",
        "summary": "Pick a custom primary accent colour for buttons, marks, and peer activity.",
        "synonyms": "accent primary colour color button brand highlight custom hex",
        "intents": ("appearance", "modify_setting"),
        "security_tier": "safe",
        "fix_hint": "Stored locally in this browser.",
        "api": None,
    },
    {
        "id": "appearance.typography",
        "page": "typography",
        "group": "appearance",
        "title": "Typography & fonts",
        "summary": "Configure UI sans font, monospace code font, and text size.",
        "synonyms": "font typography mono code sans serif size text readable",
        "intents": ("appearance", "modify_setting"),
        "security_tier": "safe",
        "fix_hint": "Stored locally in this browser.",
        "api": None,
    },
    {
        "id": "appearance.css",
        "page": "css",
        "group": "appearance",
        "title": "Custom CSS & import/export",
        "summary": "Add custom styles, export/import your look, or reset appearance defaults.",
        "synonyms": "css style custom export import reset defaults theme backup",
        "intents": ("appearance", "modify_setting"),
        "security_tier": "safe",
        "fix_hint": "Use Reset defaults if custom CSS broke layout.",
        "api": None,
    },
    {
        "id": "interface.layout",
        "page": "layout",
        "group": "interface",
        "title": "Layout, dark/light mode & room view",
        "summary": "Switch light/dark mode, density, 24h clock, and default room view (Cockpit vs Canvas).",
        "synonyms": "layout dark light mode density compact clock 24h room view cockpit canvas sidebar",
        "intents": ("appearance", "modify_setting"),
        "security_tier": "safe",
        "fix_hint": "Stored locally in this browser.",
        "api": None,
    },
    {
        "id": "keys.shortcuts",
        "page": "keys",
        "group": "keys",
        "title": "Keyboard shortcuts",
        "summary": "View command palette (⌘K), settings (⌘,), and room navigation hotkeys.",
        "synonyms": "keyboard shortcut hotkey cmd k command palette bindings keys",
        "intents": ("navigation",),
        "security_tier": "read_only",
        "fix_hint": "Use ⌘K (Ctrl+K) to jump anywhere or ⌘, (Ctrl+,) for Settings.",
        "api": None,
    },
    {
        "id": "about.info",
        "page": "about",
        "group": "about",
        "title": "About & hub build",
        "summary": "Inspect hub build version, installed themes, and open-source credits.",
        "synonyms": "about version build info licence credits libraries",
        "intents": ("navigation",),
        "security_tier": "read_only",
        "fix_hint": "Read-only version and build information.",
        "api": None,
    },
)

INTENT_KEYWORDS = {
    "troubleshoot_fix": frozenset({"fix", "broken", "error", "fail", "failed", "offline", "down", "issue", "problem", "repair", "reset", "stuck", "help", "why", "unreachable"}),
    "security_audit": frozenset({"security", "secure", "passkey", "revoke", "approval", "approve", "deny", "witness", "session", "weak", "auth", "audit", "trust", "device", "lock"}),
    "appearance": frozenset({"theme", "dark", "light", "mono", "color", "colour", "accent", "font", "css", "layout", "look", "compact"}),
    "modify_setting": frozenset({"set", "change", "enable", "disable", "turn", "switch", "mute", "unmute", "configure", "update", "toggle", "edit", "modify"}),
}


def _clean_query(raw):
    if not isinstance(raw, str):
        return "", False
    cleaned = "".join(ch for ch in raw if ord(ch) >= 32)[:MAX_QUERY].strip()
    flagged = bool(SUSPICIOUS_RE.search(cleaned))
    return cleaned, flagged


def _bigrams(s):
    if len(s) < 2:
        return {s} if s else set()
    return {s[i:i + 2] for i in range(len(s) - 1)}


def _classify_intent(tokens):
    if not tokens:
        return "navigation", 1.0
    scores = {}
    tset = set(tokens)
    for intent, kws in INTENT_KEYWORDS.items():
        hits = len(tset & kws)
        if hits:
            scores[intent] = hits
    if not scores:
        return "modify_setting", 0.55
    best = max(scores, key=scores.get)
    conf = min(0.98, 0.65 + 0.12 * scores[best])
    return best, round(conf, 2)


def classify(query, *, live_state=None, limit=8):
    """Classify a settings query and return ranked, safe targets."""
    q, flagged = _clean_query(query)
    ql = q.lower()
    tokens = TOKEN_RE.findall(ql)
    intent, confidence = _classify_intent(tokens)
    live = live_state if isinstance(live_state, dict) else {}

    # Derive live status hints per setting id (strictly non-secret counts/booleans).
    live_hints = {}
    if live.get("weak_sessions"):
        live_hints["hub.sessions"] = f"{live['weak_sessions']} weak session(s)"
    if live.get("approvals"):
        live_hints["safety.approvals"] = f"{live['approvals']} pending"
    if live.get("offline_rooms"):
        live_hints["notify.alerts"] = f"{live['offline_rooms']} offline room(s)"
    if live.get("network_mode"):
        live_hints["network.mode"] = f"mode: {live['network_mode']}"
    if live.get("push_subs") == 0:
        live_hints["notify.push"] = "not subscribed"

    scored = []
    for item in CATALOG:
        title_l = item["title"].lower()
        hay = f"{item['id']} {title_l} {item['group']} {item['summary'].lower()} {item['synonyms']}".lower()
        hay_tokens = set(TOKEN_RE.findall(hay))

        if not tokens:
            score = 1.0 if item["id"] == "overview" else 0.5
            reason = "default"
        else:
            score = 0.0
            reasons = []
            if ql == item["id"] or ql == item["page"] or ql == title_l:
                score += 6.0
                reasons.append("exact")
            elif ql in title_l:
                score += 4.0
                reasons.append("title")
            elif ql in hay:
                score += 2.2
                reasons.append("phrase")

            matched_tokens = 0
            for tok in tokens:
                if tok in hay_tokens:
                    score += 2.0 if tok in title_l else 1.3
                    matched_tokens += 1
                elif any(ht.startswith(tok) for ht in hay_tokens):
                    score += 0.9
                    matched_tokens += 1
                elif len(tok) >= 4:
                    tb = _bigrams(tok)
                    best_sim = max((len(tb & _bigrams(ht)) / max(1, len(tb | _bigrams(ht))) for ht in hay_tokens if abs(len(ht) - len(tok)) <= 2), default=0.0)
                    if best_sim >= 0.55:
                        score += 0.7 * best_sim
                        matched_tokens += 1
            if matched_tokens == len(tokens):
                score += 1.0
                reasons.append("all terms")
            if intent in item["intents"]:
                score += 0.45
                reasons.append(intent.replace("_", " "))
            if intent == "troubleshoot_fix" and item["id"] in live_hints and item["id"] != "network.mode":
                score += 1.2
                reasons.append("live issue")
            reason = ", ".join(dict.fromkeys(reasons)) if reasons else "related"

        if not tokens or score >= 0.75:
            scored.append((score, {
                "id": item["id"],
                "page": item["page"],
                "group": item["group"],
                "title": item["title"],
                "summary": item["summary"],
                "security_tier": item["security_tier"],
                "fix_hint": item["fix_hint"],
                "api": item["api"],
                "live_status": live_hints.get(item["id"]),
                "score": round(score, 2),
                "match_reason": reason,
            }))

    scored.sort(key=lambda x: (-x[0], x[1]["id"]))
    return {
        "query": q,
        "intent": intent,
        "confidence": confidence,
        "flagged": flagged,
        "security_note": "Read-only settings classifier; modifying step-up settings still requires a passkey touch.",
        "results": [r for _, r in scored[:max(1, min(limit, 15))]],
    }

/* Shared by the hub shell and every room cockpit: passkey ceremonies and step-up. */
const PB = (() => {
  const b64u = buf => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  const unb64u = s => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - s.length % 4) % 4)), c => c.charCodeAt(0));
  const canPasskey = () => !!(window.isSecureContext && window.PublicKeyCredential && navigator.credentials);

  async function post(path, body) {
    const r = await fetch(path, {method: "POST", headers: {"Content-Type": "application/json"}, body: JSON.stringify(body || {})});
    let j = {};
    try { j = await r.json(); } catch {}
    if (!r.ok) throw Object.assign(new Error(j.error || r.statusText || (r.status === 401 ? "Your session ended. Reload and sign in again." : r.status === 429 ? "Too many attempts. Wait a minute and try again." : r.status === 403 ? "The hub refused this (" + r.status + "). Reload the page and try again." : "The hub answered " + r.status + ". Reload and try again.")), {status: r.status, body: j});
    return j;
  }
  const friendly = e => e && e.name === "NotAllowedError" ? new Error("Cancelled, or no passkey was available on this device.") : e;

  async function register(code, name) {
    const b = await post("/api/auth/register/begin", {code});
    const o = b.publicKey;
    o.challenge = unb64u(o.challenge); o.user.id = unb64u(o.user.id);
    o.excludeCredentials = (o.excludeCredentials || []).map(c => ({...c, id: unb64u(c.id)}));
    let cred;
    try { cred = await navigator.credentials.create({publicKey: o}); } catch (e) { throw friendly(e); }
    return post("/api/auth/register/finish", {ticket: b.ticket, name, response: {
      clientDataJSON: b64u(cred.response.clientDataJSON), attestationObject: b64u(cred.response.attestationObject)}});
  }

  async function assert(kind) {
    const b = await post(`/api/auth/${kind}/begin`);
    const o = b.publicKey;
    o.challenge = unb64u(o.challenge);
    o.allowCredentials = o.allowCredentials.map(c => ({...c, id: unb64u(c.id)}));
    let cred;
    try { cred = await navigator.credentials.get({publicKey: o}); } catch (e) { throw friendly(e); }
    return post(`/api/auth/${kind}/finish`, {ticket: b.ticket, id: cred.id, response: {
      clientDataJSON: b64u(cred.response.clientDataJSON), authenticatorData: b64u(cred.response.authenticatorData), signature: b64u(cred.response.signature)}});
  }

  const login = () => assert("login");
  const stepup = () => assert("stepup");
  const weak = (code, name) => post("/api/auth/weak", {code, name});

  /* fetch that transparently asks for a fresh passkey touch when the hub says a sensitive action needs one */
  async function guarded(url, init) {
    let r = await fetch(url, init);
    if (r.status === 403) {
      const j = await r.clone().json().catch(() => ({}));
      if (j.error === "stepup") {
        if (j.weak) throw new Error("This needs a passkey session. Plain-HTTP sessions can't approve it unless you are on the hub machine itself.");
        await stepup();
        r = await fetch(url, init);
      }
    }
    return r;
  }
  /* Behind Cloudflare Access the sign-in expires. From then on every call is redirected to the Access login page, which the page's security
     policy blocks, so the app just looks broken. Ask the hub for something harmless without following redirects: an opaque redirect means
     the Access session is gone. Say so, with a button that signs in again (a reload goes through the Access login and comes back here). */
  function watchSession() {
    let note = null;
    const hide = () => { if (note) { note.remove(); note = null; } };
    const show = () => {
      if (note || !document.body) return;
      note = document.createElement("div");
      note.setAttribute("role", "alert");
      note.style.cssText = "position:fixed;left:12px;right:12px;bottom:12px;z-index:2147483000;display:flex;gap:12px;align-items:center;justify-content:space-between;flex-wrap:wrap;padding:12px 14px;background:#1C1E1A;color:#E5E8DD;border:1px solid #CBD8A2;font:13px/1.4 system-ui,sans-serif;box-shadow:0 6px 24px rgba(0,0,0,.4)";
      const t = document.createElement("span");
      t.textContent = "Your Cloudflare sign-in expired, so this page can no longer reach the hub.";
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = "Sign in again";
      b.style.cssText = "font:600 12px ui-monospace,Menlo,monospace;text-transform:uppercase;letter-spacing:.06em;padding:7px 12px;background:#CBD8A2;color:#1B2110;border:0;cursor:pointer";
      b.addEventListener("click", () => location.reload());
      note.append(t, b);
      document.body.append(note);
    };
    const check = async () => {
      if (document.hidden) return;
      try {
        const r = await fetch("/api/auth/state", {cache: "no-store", redirect: "manual", credentials: "same-origin"});
        if (r.type === "opaqueredirect" || r.status === 0) show(); else hide();
      } catch { /* offline or the hub is restarting: not an expired sign-in */ }
    };
    setInterval(check, 45000);
    document.addEventListener("visibilitychange", check);
    window.addEventListener("focus", check);
    window.addEventListener("online", check);
  }
  if (typeof document !== "undefined" && location.protocol.startsWith("http")) watchSession();
  return {b64u, unb64u, canPasskey, post, register, login, stepup, weak, guarded};
})();

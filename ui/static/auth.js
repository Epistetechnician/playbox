/* Shared by the hub shell and every room cockpit: passkey ceremonies and step-up. */
const PB = (() => {
  const b64u = buf => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  const unb64u = s => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - s.length % 4) % 4)), c => c.charCodeAt(0));
  const canPasskey = () => !!(window.isSecureContext && window.PublicKeyCredential && navigator.credentials);

  async function post(path, body) {
    const r = await fetch(path, {method: "POST", headers: {"Content-Type": "application/json"}, body: JSON.stringify(body || {})});
    let j = {};
    try { j = await r.json(); } catch {}
    if (!r.ok) throw Object.assign(new Error(j.error || r.statusText), {status: r.status, body: j});
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
  return {b64u, unb64u, canPasskey, post, register, login, stepup, weak, guarded};
})();

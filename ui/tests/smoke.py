#!/usr/bin/env python3
"""Browser smoke test of the hub's pages: a throwaway hub, driven by headless Chromium through Playwright.

    pip install playwright            # CI uses the Chrome that GitHub's runners already have (channel "chrome"); no browser download
    python3 ui/tests/smoke.py

Covers: the sign-in page renders, pairing signs in, the Settings dialog opens and switches a theme, markdown from other members renders
safely (nothing executes), and the Research list and a run's detail load. Environment:
    PLAYBOX_CHROME=/path/to/chrome   use this browser (default: Playwright's "chrome" channel, then its own Chromium)
    PLAYBOX_SMOKE_REQUIRE=1          fail instead of skipping when Playwright or a browser is missing (CI sets it)
"""
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
UI = os.path.dirname(HERE)
REQUIRE = os.environ.get("PLAYBOX_SMOKE_REQUIRE") == "1"
fails = []


def check(name, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + name + ("" if cond else f"  {detail}"))
    if not cond:
        fails.append(name)


def skip(why):
    print(("FAIL " if REQUIRE else "SKIP ") + "smoke: " + why)
    sys.exit(1 if REQUIRE else 0)


try:
    from playwright.sync_api import sync_playwright
except ImportError:
    skip("Playwright is not installed (pip install playwright)")


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def start_hub():
    home, port = tempfile.mkdtemp(prefix="pbsmoke-"), free_port()
    mcp = os.environ.get("PLAYBOX_MCP") or shutil.which("playbox-mcp") or "/bin/true"     # no room is opened, so the sidecar is never started
    p = subprocess.Popen([sys.executable, os.path.join(UI, "playbox_hub.py"), "--port", str(port), "--home", home, "--mcp", mcp],
                         stdout=subprocess.PIPE, text=True, env={**os.environ, "PLAYBOX_HUB_NO_MACHINE": "1", "PLAYBOX_HUB_FRESH": "2"})
    code = None
    for _ in range(40):
        line = p.stdout.readline()
        if not line:
            break
        m = re.search(r"/pair#(\S+)", line)
        if m:
            code = m.group(1)
            break
    for _ in range(100):        # the code is printed just before the socket is served
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.5).close()
            break
        except OSError:
            time.sleep(0.1)
    return p, home, port, code


def launch(pw):
    exe = os.environ.get("PLAYBOX_CHROME")
    attempts = [{"executable_path": exe}] if exe else [{"channel": "chrome"}, {}]
    err = None
    for kw in attempts:
        try:
            return pw.chromium.launch(headless=True, **kw)
        except Exception as e:      # noqa: BLE001 - whatever Playwright says is the diagnostic
            err = e
    skip(f"no browser to drive: {str(err)[:200]}")


def main():
    hub, home, port, code = start_hub()
    base = f"http://localhost:{port}"      # the hub sends 127.0.0.1 to a "use localhost" page: passkeys need a name
    try:
        check("the throwaway hub printed a pairing code", bool(code))
        with sync_playwright() as pw:
            br = launch(pw)
            ctx = br.new_context(viewport={"width": 1280, "height": 900})
            page = ctx.new_page()
            page.set_default_timeout(15000)
            errors = []
            page.on("pageerror", lambda e: errors.append(str(e)))
            dialogs = []
            page.on("dialog", lambda d: (dialogs.append(d.message), d.dismiss()))

            # 1. sign-in: a hub that nobody has paired with says so; the pairing link signs in
            page.goto(base + "/", wait_until="domcontentloaded")
            page.wait_for_selector("text=Pair this device", timeout=15000)
            check("sign-in: the page renders and asks for a pairing", "Pair this device" in page.inner_text("body"))
            cdp = ctx.new_cdp_session(page)         # a virtual authenticator stands in for the fingerprint or device PIN
            cdp.send("WebAuthn.enable")
            cdp.send("WebAuthn.addVirtualAuthenticator", {"options": {"protocol": "ctap2", "transport": "internal", "hasResidentKey": True,
                                                                      "hasUserVerification": True, "isUserVerified": True, "automaticPresenceSimulation": True}})
            page.goto(base + "/pair#" + code, wait_until="domcontentloaded")
            page.wait_for_selector("#go", timeout=15000)
            check("sign-in: the pairing link offers to create a passkey", page.locator("#go").is_visible())
            page.click("#go")
            page.wait_for_selector("#railnav .tab", timeout=15000)
            check("sign-in: pairing lands on the home page with its tabs", page.locator("#railnav .tab").count() >= 5)

            # 2. settings: open, switch a theme
            page.click('[data-act="settings"]')
            page.wait_for_selector("dialog.pbs[open]", timeout=5000)
            check("settings: the dialog opens", page.locator("dialog.pbs[open]").count() == 1)
            page.click('dialog.pbs .pg:has-text("Appearance"), dialog.pbs [data-p="appearance"], dialog.pbs [data-p="theme"]', timeout=3000) if page.locator("dialog.pbs .tc").count() == 0 else None
            cards = page.locator('dialog.pbs .tc[aria-pressed="false"]')
            check("settings: the Appearance page offers themes", cards.count() > 1, "no theme cards found")
            before = page.evaluate("PBTheme.state().skin")
            target = cards.first.get_attribute("data-v")
            cards.first.click()
            after = page.evaluate("PBTheme.state().skin")
            stored = page.evaluate("localStorage.getItem('pb.skin')")
            check("settings: choosing a theme switches it and remembers it", after == target and after != before and stored == target, f"{before} -> {after} ({target}) stored={stored}")
            check("settings: the page's colours changed with it", page.evaluate("getComputedStyle(document.documentElement).getPropertyValue('--bg').trim()") != "")
            page.keyboard.press("Escape")
            page.wait_for_selector("dialog.pbs[open]", state="detached", timeout=3000)

            # 3. notes: markdown from another member never executes
            evil = ("# Title\n<img src=x onerror=\"window.__pwned=1\"> <script>window.__pwned=1</script>\n"
                    "[bad](javascript:window.__pwned=1) [ok](https://example.com) ![i](https://evil.example/x.png)\n**bold** `code`")
            page.add_script_tag(url="/markdown.js")      # the Notes/Work pages load it; the home page does not
            page.evaluate("""async (src) => {
                const el = document.createElement('div'); el.id = 'mdprobe'; document.body.appendChild(el);
                PBMd.into(el, src);
            }""", evil)
            page.wait_for_timeout(1500)       # DOMPurify loads from /vendor/ and the text is rendered again
            md = page.evaluate("""() => { const e = document.getElementById('mdprobe');
                return {html: e.innerHTML, pwned: !!window.__pwned, scripts: e.querySelectorAll('script').length, imgs: e.querySelectorAll('img').length,
                        handlers: [...e.querySelectorAll('*')].filter(n => [...n.attributes].some(a => /^on/i.test(a.name))).length,
                        jshref: [...e.querySelectorAll('a')].filter(a => /^javascript:/i.test(a.getAttribute('href') || '')).length,
                        bold: e.querySelectorAll('b,strong').length, h: e.querySelectorAll('h1,h2,h3').length}; }""")
            check("notes: markdown renders (heading, bold)", md["h"] >= 1 and md["bold"] >= 1, md["html"][:200])
            check("notes: nothing from the text executes or loads", not md["pwned"] and md["scripts"] == 0 and md["imgs"] == 0 and md["handlers"] == 0 and md["jshref"] == 0, str(md))

            # 4. research: list, then a run
            page.click('#railnav [data-tab="research"]')
            page.wait_for_selector("table.rtab tr[data-run]", timeout=10000)
            n = page.locator("table.rtab tr[data-run]").count()
            check("research: the list shows the runs on disk", n >= 1 and n == len([f for f in os.listdir(os.path.join(UI, "research")) if f.endswith(".json")]), str(n))
            page.locator("table.rtab tr[data-run]").first.click()
            page.wait_for_selector(".rtabs", timeout=10000)
            check("research: a run opens with its its tabs and a title", page.locator('.rtabs [role="tab"]').count() >= 6 and page.locator(".rh .rt").first.inner_text().strip() != "",
                  f'{page.locator(".rtabs [role=tab]").count()} tabs, {page.locator(".rh .rt").count()} titles')
            page.click('[data-rtab="results"]')
            page.wait_for_selector("table.rtab tbody tr", timeout=5000)
            check("research: the Results tab shows a table", page.locator("table.rtab tbody tr").count() >= 1)

            # 5. geometry (Refined vs Expressive), QR pairing, and condensed mobile bottom bar
            geom = page.evaluate("""() => {
                PBTheme.set('style', 'refined');
                const cardRefined = getComputedStyle(document.querySelector('.card')).borderTopLeftRadius;
                const ledRefined = getComputedStyle(document.querySelector('.led')).borderTopLeftRadius;
                PBTheme.set('style', 'expressive');
                const cardExpressive = getComputedStyle(document.querySelector('.card')).borderTopLeftRadius;
                PBTheme.set('style', 'refined');
                return {cardRefined, ledRefined, cardExpressive};
            }""")
            check("geometry: Refined keeps cards sharp while LEDs stay circular and Expressive restores radius",
                  geom["cardRefined"] == "0px" and geom["ledRefined"] != "0px" and geom["cardExpressive"] != "0px", str(geom))

            page.evaluate("pairDevice()")
            page.wait_for_selector("#sheet[open] svg.pbqr", timeout=5000)
            check("qr: device pairing sheet renders a client-side SVG QR code", page.locator("#sheet[open] svg.pbqr path").count() >= 1)
            page.keyboard.press("Escape")

            page.set_viewport_size({"width": 390, "height": 844})
            page.wait_for_timeout(150)
            mob_tabs = page.locator("#bottom .tab").count()
            check("mobile: Hub bottom bar is condensed to 5 spacious items", mob_tabs == 5, str(mob_tabs))
            page.click('#bottom [data-act="more"]')
            page.wait_for_selector("#sheet[open]", timeout=3000)
            check("mobile: More opens the secondary navigation sheet", page.locator('#sheet[open] [data-mnav="integrations"]').count() == 1)
            page.keyboard.press("Escape")
            page.set_viewport_size({"width": 1280, "height": 900})

            # 6. unified room shell (Canvas + Cockpit switcher, drawer, ⌘K palette, trackpad pan, inline composer)
            rid = page.evaluate("""async () => {
                const r = await fetch('/api/rooms', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({label: 'Smoke Lab', kind: 'mine'})});
                return (await r.json()).id;
            }""")
            page.goto(f"{base}/r/{rid}/canvas", wait_until="domcontentloaded")
            page.wait_for_selector("#switcher .pbsw", timeout=10000)
            check("canvas: unified Canvas/Cockpit switcher, Rooms drawer button and ⌘K button mount",
                  page.locator("#switcher .pbsw a").count() == 2 and page.locator("#roomsbtn").is_visible() and page.locator("#palbtn").is_visible())
            page.click("#roomsbtn")
            page.wait_for_selector("#pbdrawer.open", timeout=3000)
            check("canvas: Rooms drawer opens and lists the room", "Smoke Lab" in page.inner_text("#pbdrawer"))
            page.keyboard.press("Escape")

            page.click("#palbtn")
            page.wait_for_selector("dialog.pbpal[open]", timeout=3000)
            check("canvas: universal ⌘K command palette opens", page.locator("dialog.pbpal[open] li").count() >= 5)
            page.keyboard.press("Escape")

            # Wait briefly for the canvas model (if playbox-mcp is present) or seed a minimal model when running against /bin/true
            page.evaluate("""() => {
                if (!M) {
                    M = {room: {label: 'Smoke Lab', kind: 'mine', chain: {ok: true, entries: 1}, holds: []}, admin: true, offline: false, now: Date.now(),
                         members: [], feed: [], artifacts: [], changelog: [], cards: [], cols: ['todo', 'doing', 'review', 'done'], metrics: [],
                         stats: {messages: 0, activity: [{n: 0}], per_member: {}, artifacts: {verified: 0}, cards: {todo: 0, doing: 0, review: 0, done: 0}}};
                    drawWorld(); fitAll(false);
                }
            }""")
            pan_res = page.evaluate("""() => {
                const before = {...view};
                document.getElementById('stage').dispatchEvent(new WheelEvent('wheel', {deltaX: 40, deltaY: 30, bubbles: true, cancelable: true}));
                const afterPan = {...view};
                document.getElementById('stage').dispatchEvent(new WheelEvent('wheel', {deltaY: -50, ctrlKey: true, clientX: 400, clientY: 300, bubbles: true, cancelable: true}));
                const afterZoom = {...view};
                return {before, afterPan, afterZoom};
            }""")
            check("canvas: plain wheel pans and ctrl+wheel zooms",
                  pan_res["afterPan"]["x"] != pan_res["before"]["x"] and pan_res["afterPan"]["k"] == pan_res["before"]["k"]
                  and pan_res["afterZoom"]["k"] > pan_res["afterPan"]["k"], str(pan_res))

            page.keyboard.press("3")
            check("canvas: numeric hotkey drills into panel 03 (Universal feed) with inline composer",
                  page.locator('.panel.open[data-p="feed"] form[data-csay] input[name="text"]').count() == 1
                  and page.locator('.panel.open[data-p="feed"] [data-cfeedq]').count() == 1)
            c_ux = page.evaluate("""() => {
                const samplePatch = [
                    'diff --git a/src/a.py b/src/a.py', '--- a/src/a.py', '+++ b/src/a.py', '@@ -1 +1,2 @@', '-old', '+new1', '+new2',
                    'diff --git a/src/b.py b/src/b.py', '--- a/src/b.py', '+++ b/src/b.py', '@@ -1,2 +1 @@', '-drop1', '-drop2', '+keep'
                ].join('\\n');
                M.members = [{id: 'm111111', name: 'Ada', present: true, msgs: 1}, {id: 'm222222', name: 'Grace', present: true, msgs: 1}];
                M.feed = [{seq: 1, from: 'm111111', text: 'Investigating kernel race', ms: Date.now() - 2000},
                          {seq: 2, from: 'm222222', text: 'Benchmarking throughput', ms: Date.now() - 1000}];
                M.artifacts = [{id: 'p1', kind: 'code', status: 'verified', title: 'Patch', summary: 'Two files', body: samplePatch, revs: [{status: 'verified', from: 'm111111', ms: Date.now()}]}];
                drawWorld();
                const fq = document.querySelector('[data-cfeedq]');
                fq.focus();
                fq.value = 'Ada';
                fq.dispatchEvent(new Event('input', {bubbles: true}));
                const keptFocus = document.activeElement === fq;
                const filteredMsgs = document.querySelectorAll('.panel.open[data-p="feed"] .c-msgs .msg').length;
                openSel('artifact', 'p1');
                const inspCards = document.querySelectorAll('#insp details.diff-file').length;
                return {keptFocus, filteredMsgs, inspCards};
            }""")
            check("canvas: drilled feed filter preserves focus and artifact inspector renders collapsible diff cards",
                  c_ux["keptFocus"] and c_ux["filteredMsgs"] == 1 and c_ux["inspCards"] == 2, str(c_ux))

            page.goto(f"{base}/r/{rid}/", wait_until="domcontentloaded")
            page.wait_for_selector(".pbsw", timeout=10000)
            page.wait_for_selector("#view .view", timeout=10000)
            page.set_viewport_size({"width": 390, "height": 844})
            page.wait_for_timeout(150)
            check("cockpit: mounts unified switcher and condenses mobile tabbar to 5 items",
                  page.locator("#tabbar .nav").count() == 5)
            page.set_viewport_size({"width": 1280, "height": 900})

            page.evaluate("""() => {
                S.ok = true;
                if (!S.status || (!S.status.hosting && !S.status.joined)) {
                    S.status = {hosting: {members: ['peer11112222']}, joined: {member_id: 'self00001111', active_host: 'host00001111'}};
                }
                S.feed = [
                    {seq: 1, kind: 'say', from: 'peer11112222', name: 'Ada', text: 'Fixed parser edge case', time_ms: Date.now() - 3000},
                    {seq: 2, kind: 'say', from: 'self00001111', name: 'You', text: 'Running sandbox suite now', time_ms: Date.now() - 1000},
                ];
                S.seq = 2;
                show('feed');
            }""")
            page.wait_for_selector("#feedq", timeout=5000)
            page.keyboard.press("/")
            check("cockpit: Feed renders #feedq and '/' shortcut focuses it",
                  page.evaluate("document.activeElement && document.activeElement.id === 'feedq'"))
            page.keyboard.type("Ada")
            check("cockpit: #feedq filters messages in place and updates count + clear button",
                  page.locator("#feedlist .msg").count() == 1
                  and page.inner_text("#feedqmeta").strip() == "1 of 2"
                  and page.locator("#feedqclear").is_visible()
                  and page.evaluate("document.activeElement && document.activeElement.id === 'feedq'"))
            page.fill("#feedq", "no-such-token-xyz")
            check("cockpit: #feedq shows empty match message when 0 match",
                  "No messages match" in page.inner_text("#feedlist"))
            page.click("#feedqclear")
            check("cockpit: #feedqclear restores full feed",
                  page.locator("#feedlist .msg").count() == 2 and not page.locator("#feedqclear").is_visible())

            patch_check = page.evaluate("""() => {
                const patch = [
                    'diff --git a/ui/a.js b/ui/a.js', 'index 111..222 100644', '--- a/ui/a.js', '+++ b/ui/a.js',
                    '@@ -1,2 +1,3 @@', ' const x = 1;', '-const y = 2;', '+const y = 3;', '+const z = 4;',
                    'diff --git a/ui/b.js b/ui/b.js', '--- a/ui/b.js', '+++ b/ui/b.js',
                    '@@ -1 +1 @@', '-export default 0;', '+export default 1;'
                ].join('\\n');
                const holder = document.createElement('div');
                holder.innerHTML = renderResult({exit_code: 0, timed_out: false, patch, patch_bytes: patch.length});
                document.body.appendChild(holder);
                const cards = [...holder.querySelectorAll('details.diff-file')];
                const out = {
                    count: cards.length,
                    allOpen: cards.every(c => c.open),
                    hasPatchPre: !!holder.querySelector('#patchpre'),
                    hasCopyBtn: !!holder.querySelector('[data-act="copypatch"]'),
                    firstSummary: cards[0] ? cards[0].querySelector('summary').textContent.replace(/\\s+/g, ' ').trim() : '',
                    addsSpan: holder.querySelectorAll('pre.patchpre .add').length,
                    delsSpan: holder.querySelectorAll('pre.patchpre .del').length,
                };
                holder.remove();
                return out;
            }""")
            check("cockpit: renderResult renders collapsible per-file .diff-file cards with +adds/-dels and syntax spans",
                  patch_check["count"] == 2 and patch_check["allOpen"] and patch_check["hasPatchPre"]
                  and patch_check["hasCopyBtn"] and "ui/a.js" in patch_check["firstSummary"]
                  and "+2" in patch_check["firstSummary"] and "-1" in patch_check["firstSummary"]
                  and patch_check["addsSpan"] == 3 and patch_check["delsSpan"] == 2, str(patch_check))

            check("no uncaught page errors", not errors, "; ".join(errors)[:300])
            check("no unexpected alert/confirm dialogs", not dialogs, str(dialogs))
            print(f"\n{len(fails)} failed" if fails else "\nall passed", flush=True)
            hub.kill()
            shutil.rmtree(home, ignore_errors=True)
            os._exit(1 if fails else 0)      # closing the browser can hang on some runners; everything is checked and cleaned up by now
    finally:
        hub.kill()
        hub.wait(timeout=10)
        hub.stdout.close()
        shutil.rmtree(home, ignore_errors=True)
    print(f"\n{len(fails)} failed" if fails else "\nall passed")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()

"""Exercise the built token-entry UI against the isolated real gateway."""
import argparse
import json
from pathlib import Path
from playwright.sync_api import sync_playwright, expect

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--base-url", default="http://127.0.0.1:8080")
parser.add_argument("--output", type=Path)
arguments = parser.parse_args()
TOKEN = "poc-isolated-test-token-not-a-real-credential"
BASE = arguments.base_url
report = []
with sync_playwright() as playwright:
    for engine in ("chromium", "firefox"):
        browser = getattr(playwright, engine).launch(headless=True)
        context = browser.new_context()
        page = context.new_page()
        errors = []
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.goto(BASE)
        field = page.get_by_label("Gateway token", exact=True)
        expect(field).to_be_visible()
        expect(field).to_have_attribute("type", "password")
        expect(page.locator(".skill-card")).to_have_count(0)
        field.fill("   ")
        page.get_by_role("button", name="Connect", exact=True).click()
        expect(page.get_by_role("alert")).to_have_text("Enter a gateway token.")
        field.fill("invalid")
        page.get_by_role("button", name="Connect", exact=True).click()
        expect(page.get_by_role("alert")).to_contain_text("Token rejected")
        assert page.evaluate("localStorage.getItem('zeroclaw_token')") is None
        expect(page.locator(".skill-card")).to_have_count(0)
        field.fill("  " + TOKEN + "  ")
        field.press("Enter")
        expect(page.locator(".skill-card")).to_have_count(2)
        assert page.evaluate("localStorage.getItem('zeroclaw_token')") == TOKEN
        page.get_by_role("combobox", name="Agent", exact=True).select_option("beta")
        expect(page.locator(".skill-name")).to_have_text("beta-guide")
        page.get_by_role("combobox", name="Agent", exact=True).select_option("alpha")
        search = page.get_by_role("textbox", name="Search skills…", exact=True)
        search.fill("alpha-guide")
        expect(page.locator(".skill-card")).to_have_count(1)
        page.locator("button.card-body").click()
        expect(page.locator(".skill-body")).to_contain_text("# Alpha Guide")
        search.fill("")
        page.get_by_role("button", name="Reload", exact=True).click()
        expect(page.locator(".skill-card")).to_have_count(2)
        page.reload()
        expect(page.locator(".skill-card")).to_have_count(2)
        page.evaluate("() => { window.originalRemoveItem = Storage.prototype.removeItem; Storage.prototype.removeItem = () => { throw new Error('blocked'); }; }")
        page.get_by_role("button", name="Disconnect / change token", exact=True).click()
        expect(page.get_by_role("alert")).to_contain_text("Browser storage is unavailable")
        expect(page.locator(".skill-card")).to_have_count(2)
        page.evaluate("() => { Storage.prototype.removeItem = window.originalRemoveItem; }")
        page.get_by_role("button", name="Disconnect / change token", exact=True).click()
        expect(field).to_be_visible()
        expect(field).to_have_value("")
        expect(page.locator(".skill-card")).to_have_count(0)
        assert page.evaluate("localStorage.getItem('zeroclaw_token')") is None
        waiting = []
        page.route("**/api/config/agent-options", lambda route: waiting.append(route))
        field.fill(TOKEN)
        page.get_by_role("button", name="Connect", exact=True).click()
        expect(field).to_be_disabled()
        expect(page.get_by_role("button", name="Checking connection…", exact=True)).to_be_disabled()
        page.wait_for_function("document.querySelector('#gateway-token').disabled")
        assert len(waiting) == 1
        waiting[0].fulfill(status=401, body="denied")
        expect(field).to_be_enabled()
        page.unroute("**/api/config/agent-options")
        for status, body, message in ((502, "offline", "Cannot validate"),
                                      (403, "denied", "Token rejected"),
                                      (200, "{}", "Cannot validate")):
            page.route("**/api/config/agent-options",
                       lambda route, request, status=status, body=body: route.fulfill(status=status, body=body))
            field.fill(TOKEN)
            page.get_by_role("button", name="Connect", exact=True).click()
            expect(page.get_by_role("alert")).to_contain_text(message)
            assert page.evaluate("localStorage.getItem('zeroclaw_token')") is None
            expect(page.locator(".skill-card")).to_have_count(0)
            page.unroute("**/api/config/agent-options")
        context.close()
        context = browser.new_context()
        context.on("page", lambda page: page.on("pageerror", lambda error: errors.append(str(error))))
        context.add_init_script("localStorage.setItem('zeroclaw_token', 'invalid')")
        page = context.new_page()
        page.goto(BASE)
        expect(page.get_by_label("Gateway token", exact=True)).to_be_visible()
        expect(page.get_by_role("alert")).to_contain_text("Token rejected")
        expect(page.locator(".skill-card")).to_have_count(0)
        context.close()
        context = browser.new_context()
        context.on("page", lambda page: page.on("pageerror", lambda error: errors.append(str(error))))
        context.add_init_script("Storage.prototype.setItem = () => { throw new Error('blocked'); }")
        page = context.new_page()
        page.goto(BASE)
        page.get_by_label("Gateway token", exact=True).fill(TOKEN)
        page.get_by_role("button", name="Connect", exact=True).click()
        expect(page.get_by_role("alert")).to_contain_text("Browser storage is unavailable")
        expect(page.locator(".skill-card")).to_have_count(0)
        assert errors == [], errors
        report.append({"browser": engine, "valid_connect": True, "invalid_rejected": True,
                       "blank_rejected": True, "reload": True, "disconnect": True,
                       "gateway_unavailable": True, "stale_token": True,
                       "storage_blocked": True, "forbidden_rejected": True,
                       "malformed_response_rejected": True, "skills_interactions": True,
                       "pending_form_disabled": True, "failed_disconnect_reported": True,
                       "javascript_errors": errors})
        context.close()
        browser.close()
print(json.dumps(report, indent=2))
if arguments.output:
    arguments.output.write_text(json.dumps(report, indent=2))

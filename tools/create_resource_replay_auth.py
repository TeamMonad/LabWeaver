#!/usr/bin/env python3
"""Create private BFF sessions through the public OIDC authorization flow."""
from __future__ import annotations
import argparse, html.parser, http.cookiejar, json, os, ssl, urllib.parse, urllib.request
from pathlib import Path

class Error(Exception): pass
class Form(html.parser.HTMLParser):
    def __init__(self): super().__init__(); self.action=None; self.fields={}
    def handle_starttag(self, tag, attrs):
        a=dict(attrs)
        if tag == "form" and a.get("id") == "kc-form-login": self.action=a.get("action")
        if tag == "input" and a.get("name"): self.fields[a["name"]]=a.get("value", "")

IDENTITIES = {
    "teacher": "sprint2-teacher",
    "student": "sprint2-student",
    "platform-admin": "sprint2-admin",
}


def identity_seed(path: Path) -> dict[str, tuple[str, str]]:
    st = path.stat()
    if not path.is_file() or st.st_mode & 0o077:
        raise Error("LW_RESOURCE_REPLAY_AUTH_IDENTITY_SEED_INVALID")
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
        users = document["users"]
    except (OSError, UnicodeError, ValueError, KeyError, TypeError) as exc:
        raise Error("LW_RESOURCE_REPLAY_AUTH_IDENTITY_SEED_INVALID") from exc
    if not isinstance(users, list):
        raise Error("LW_RESOURCE_REPLAY_AUTH_IDENTITY_SEED_INVALID")
    result: dict[str, tuple[str, str]] = {}
    for role, expected_username in IDENTITIES.items():
        matches = [entry for entry in users if isinstance(entry, dict) and entry.get("username") == expected_username]
        if len(matches) != 1:
            raise Error("LW_RESOURCE_REPLAY_AUTH_IDENTITY_SEED_INVALID")
        credentials = matches[0].get("credentials")
        passwords = [entry for entry in credentials if isinstance(entry, dict) and entry.get("type") == "password"] if isinstance(credentials, list) else []
        if len(passwords) != 1 or not isinstance(passwords[0].get("value"), str):
            raise Error("LW_RESOURCE_REPLAY_AUTH_IDENTITY_SEED_INVALID")
        value = passwords[0]["value"]
        if not value or "\n" in value or "\r" in value or "\0" in value:
            raise Error("LW_RESOURCE_REPLAY_AUTH_IDENTITY_SEED_INVALID")
        result[role] = (expected_username, value)
    return result

def login(base: str, cas: list[Path], role: str, username: str, secret: str, destination: Path) -> None:
    jar=http.cookiejar.CookieJar(); context=ssl.create_default_context()
    for ca in cas: context.load_verify_locations(cafile=str(ca))
    opener=urllib.request.build_opener(urllib.request.HTTPSHandler(context=context), urllib.request.HTTPCookieProcessor(jar))
    try:
        response=opener.open(base + "/auth/login?return_to=%2F", timeout=30); page=response.read().decode("utf-8", "strict")
        form=Form(); form.feed(page)
        if not form.action or not form.action.startswith("https://"): raise Error("LW_RESOURCE_REPLAY_AUTH_OIDC_FORM_INVALID")
        form.fields.update({"username":username,"password":secret})
        response=opener.open(urllib.request.Request(form.action, data=urllib.parse.urlencode(form.fields).encode(), method="POST"), timeout=30); response.read()
        csrf=opener.open(base + "/api/v1/auth/csrf", timeout=30); payload=json.loads(csrf.read())
        if not isinstance(payload,dict) or not isinstance(payload.get("token",payload.get("csrfToken")),str): raise Error("LW_RESOURCE_REPLAY_AUTH_CSRF_INVALID")
    except urllib.error.HTTPError as exc:
        raise Error(f"LW_RESOURCE_REPLAY_AUTH_OIDC_{role.upper().replace('-', '_')}_HTTP_{exc.code}") from exc
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise Error(f"LW_RESOURCE_REPLAY_AUTH_OIDC_{role.upper().replace('-', '_')}_FAILED") from exc
    cookies=[{"name":c.name,"value":c.value,"domain":c.domain,"path":c.path,"expires":c.expires or -1,"httpOnly":False,"secure":bool(c.secure),"sameSite":"Lax"} for c in jar if urllib.parse.urlparse(base).hostname and c.domain.lstrip(".").endswith(urllib.parse.urlparse(base).hostname)]
    if not cookies: raise Error("LW_RESOURCE_REPLAY_AUTH_SESSION_INVALID")
    destination.write_text(json.dumps({"cookies":cookies,"origins":[]},separators=(",",":")),encoding="utf-8"); os.chmod(destination,0o600)

def main() -> int:
    p=argparse.ArgumentParser(); p.add_argument("--base-url",required=True); p.add_argument("--trusted-ca",type=Path,required=True,action="append"); p.add_argument("--output-root",type=Path,required=True); p.add_argument("--identity-seed",type=Path,required=True)
    a=p.parse_args()
    if not a.base_url.startswith("https://") or urllib.parse.urlparse(a.base_url).path not in ("", "/") or not all(ca.is_file() for ca in a.trusted_ca): raise SystemExit("LW_RESOURCE_REPLAY_AUTH_INPUT_INVALID")
    try:
        identities = identity_seed(a.identity_seed)
        a.output_root.mkdir(mode=0o700,parents=True,exist_ok=True); os.chmod(a.output_root,0o700)
        for role, (username, secret) in identities.items():
            login(a.base_url.rstrip("/"),a.trusted_ca,role,username,secret,a.output_root/f"{role}.json")
        (a.output_root/"resource-replay-auth.json").write_text(json.dumps({"apiVersion":"deploy.labweaver.io/resource-replay-auth/v1","baseUrl":a.base_url.rstrip("/"),"teacherStorageState":str(a.output_root/"teacher.json"),"studentStorageState":str(a.output_root/"student.json"),"platformAdminStorageState":str(a.output_root/"platform-admin.json")},separators=(",",":")),encoding="utf-8"); os.chmod(a.output_root/"resource-replay-auth.json",0o600)
    except Error as exc: raise SystemExit(str(exc))
    return 0
if __name__ == "__main__": raise SystemExit(main())

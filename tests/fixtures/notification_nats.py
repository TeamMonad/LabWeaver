"""Render real NATS test users using the deployment's sole permission catalog."""
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "tools"))
from prepare_platform_foundation import NATS_USERS

users = [{"user": "operator", "password": "operator-fixture"}]
for identity in ("resource-service", "control-service"):
    publish, subscribe, response = NATS_USERS[identity]
    permissions = {"publish": {"allow": list(publish)}, "subscribe": {"allow": list(subscribe)}}
    if response:
        permissions["allow_responses"] = {"max": 1}
    users.append({"user": identity, "password": "identity-fixture", "permissions": permissions})
print(json.dumps({"port": 4222, "jetstream": {"store_dir": "/tmp/notification-streams"}, "authorization": {"users": users}}))

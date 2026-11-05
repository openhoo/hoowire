#!/usr/bin/env python3
"""Exercise an independently installed, unmodified Teamwire SDK against HooWire.

No Teamwire implementation is included here. Install a licensed SDK wheel into
the active Python environment, or supply --wheel when its dependencies are
already installed. All assertions use the documented SDK objects and methods.
The JSON report distinguishes failed, blocked, and unexercised functionality.
"""

from __future__ import annotations

import argparse
import asyncio
import importlib.metadata
import json
import os
from pathlib import Path
import sys
import tempfile
import time
from datetime import datetime, timedelta, timezone
from typing import Any, Callable
from urllib.parse import urlparse
from uuid import uuid4


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


class Runner:
    def __init__(self) -> None:
        self.results: list[dict[str, Any]] = []

    def case(self, name: str, methods: list[str], operation: Callable[[], Any]) -> Any:
        start = time.perf_counter()
        result: dict[str, Any] = {"name": name, "methods": methods}
        try:
            value = operation()
            result["status"] = "passed"
            return value
        except Exception as exc:
            result.update(status="failed", error=f"{type(exc).__name__}: {exc}")
            return None
        finally:
            result["duration_ms"] = round((time.perf_counter() - start) * 1000, 3)
            self.results.append(result)
            print(f"{result['status'].upper():7} {name}", file=sys.stderr, flush=True)
            if result.get("error"):
                print(f"        {result['error']}", file=sys.stderr, flush=True)

    def blocked(self, name: str, methods: list[str], reason: str) -> None:
        self.results.append(dict(name=name, methods=methods, status="blocked", reason=reason))

    def report(self, host: str, sdk_version: str, surface: list[str]) -> dict[str, Any]:
        passed_methods = sorted({m for r in self.results if r["status"] == "passed" for m in r["methods"]})
        attempted_methods = sorted({m for r in self.results if r["status"] in {"passed", "failed"} for m in r["methods"]})
        failed_methods = sorted({m for r in self.results if r["status"] == "failed" for m in r["methods"]} - set(passed_methods))
        return {
            "schema_version": 1,
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "host": host,
            "sdk_version": sdk_version,
            "python_version": sys.version.split()[0],
            "client_modified": False,
            "results": self.results,
            "counts": {s: sum(r["status"] == s for r in self.results) for s in ("passed", "failed", "blocked")},
            "passed_methods": passed_methods,
            "attempted_methods": attempted_methods,
            "failed_methods": failed_methods,
            "not_exercised": sorted(set(surface) - set(attempted_methods)),
            "not_qualified": sorted(set(surface) - set(passed_methods)),
            "method_coverage": {"documented": len(surface), "attempted": len(set(surface) & set(attempted_methods)), "passed": len(set(surface) & set(passed_methods))},
            "qualification": "Selected public Python SDK scenarios; this report does not certify complete Teamwire compatibility.",
        }


def run_sdk(args: argparse.Namespace, runner: Runner) -> None:
    from teamwire_api.connection import Connection, ConnectionConfig
    from teamwire_api.profiles import FieldUpdate, ProfileField
    from teamwire_api import AlertType, AttachmentType, LocationSharingDuration, ReactionType
    from teamwire_api.attachments import LocationSharingAttachment, PollAttachment

    marker = f"HooWire SDK conformance {uuid4()}"
    config = runner.case("connection configuration", ["ConnectionConfig.__init__"],
                         lambda: ConnectionConfig(hide_deleted_messages=False, default_timeout=3, long_timeout=5, assets_timeout=10))
    connection = runner.case("bootstrap", ["Connection.__init__"], lambda: Connection(args.appid, args.secret, args.host, config=config))
    if connection is None:
        runner.blocked("sdk operations", [], "SDK constructor failed")
        return

    with connection as conn:
        def call(name: str, method: str, operation: Callable[[], Any], check: Callable[[Any], None] | None = None) -> Any:
            def execute() -> Any:
                result = conn.exec_async(operation())
                if check:
                    check(result)
                return result
            return runner.case(name, [method, "Connection.exec_async"], execute)

        def is_list(value: Any) -> None:
            require(isinstance(value, list), "SDK did not return a list")

        own = conn.own_user
        runner.case("own user identity", ["User.full_name"], lambda: require(bool(own.uid and own.full_name()), "Missing bot identity"))
        call("permissions", "Connection.get_permissions", conn.get_permissions)
        initial_settings = call("settings", "Connection.get_settings", conn.get_settings)
        initial_receipts = getattr(initial_settings, "read_receipts_enabled", True)
        call("update settings", "Connection.update_settings", lambda: conn.update_settings(read_receipts_enabled=False))
        def read_fresh_settings() -> None:
            previous_loop = asyncio.get_event_loop()
            fresh_loop = asyncio.new_event_loop()
            try:
                with Connection(args.appid, args.secret, args.host, config=config, loop=fresh_loop) as fresh:
                    settings = fresh.exec_async(fresh.get_settings())
                    require(settings.read_receipts_enabled is False, "Settings did not persist across a fresh SDK connection")
            finally:
                if not fresh_loop.is_closed():
                    fresh_loop.close()
                asyncio.set_event_loop(previous_loop)
        runner.case("settings persisted", ["Connection.get_settings"], read_fresh_settings)
        call("restore settings", "Connection.update_settings", lambda: conn.update_settings(read_receipts_enabled=initial_receipts))
        call("users", "Users.get_list", conn.users.get_list, is_list)
        call("user by id", "Users.get_user", lambda: conn.users.get_user(own.uid), lambda u: require(u.uid == own.uid, "Wrong user id"))
        call("directory query", "Users.query", lambda: conn.users.query(own.full_name()), is_list)
        call("full directory query", "Users.full_query", lambda: conn.users.full_query(own.full_name()), is_list)
        call("organisations", "Organisations.get_list", conn.organisations.get_list, is_list)
        groups = call("circles", "Groups.get_list", conn.groups.get_list, is_list)
        roles = call("roles", "Roles.get_list", conn.roles.get_list, is_list)
        call("bot profile", "User.get_profile", own.get_profile)
        call("update bot profile", "LoggedInUser.update_profile", lambda: own.update_profile(FieldUpdate(ProfileField.STATUS, marker)))
        call("profile persisted", "User.get_profile", own.get_profile,
             lambda p: require(p.status == marker, "Profile status did not persist"))
        for group in (groups or [])[:1]:
            call("circle by id", "Groups.get_group", lambda: conn.groups.get_group(group.gid), lambda g: require(g.gid == group.gid, "Wrong circle"))
            runner.case("circle members", ["Group.get_members"], lambda: is_list(group.get_members()))
            runner.case("circle membership", ["Group.is_member"], lambda: require(isinstance(group.is_member(own), bool), "Expected bool"))
        if not groups:
            runner.blocked("circle lookup and membership", ["Groups.get_group", "Group.get_members", "Group.is_member"], "No circle visible in the fixture")
        for role in (roles or [])[:1]:
            call("role by id", "Roles.get_role", lambda: conn.roles.get_role(role.id), lambda r: require(r.id == role.id, "Wrong role"))
            call("role members", "Role.get_members", role.get_members, is_list)
        if not roles:
            runner.blocked("role lookup and members", ["Roles.get_role", "Role.get_members"], "No role visible in the fixture")

        other = None
        if args.other_user_id:
            other = call("other user", "Users.get_user", lambda: conn.users.get_user(args.other_user_id))
        chat = call("create group chat", "Chats.create_chat", lambda: conn.chats.create_chat(title=marker, users=[own], admins=[own]),
                    lambda c: require(c.title == marker and bool(c.tid), "Created chat mismatch"))
        if chat is None:
            runner.blocked("chat operations", [], "Group chat creation failed")
            return
        call("chat by id", "Chats.get_by_thread_id", lambda: conn.chats.get_by_thread_id(chat.tid), lambda c: require(c.tid == chat.tid, "Wrong chat"))
        call("chat by title", "Chats.get_by_title", lambda: conn.chats.get_by_title(marker),
             lambda cs: require(any(c.tid == chat.tid for c in cs), "Created chat not found by title"))
        call("chat subscriptions", "Chats.get_subscriptions", conn.chats.get_subscriptions,
             lambda cs: require(any(c.tid == chat.tid for c in cs), "Created chat not subscribed"))
        runner.case("direct user membership", ["Chat.get_user_members"], lambda: require(own.uid in {u.uid for u in chat.get_user_members()}, "Bot missing from direct membership"))
        runner.case("direct circle membership", ["Chat.get_group_members"], lambda: is_list(chat.get_group_members()))
        runner.case("direct role membership", ["Chat.get_role_members"], lambda: is_list(chat.get_role_members()))
        runner.case("chat admins", ["Chat.get_admins"], lambda: require(own.uid in {u.uid for u in chat.get_admins()}, "Creator is not an admin"))
        text = marker + "\nGrüße aus Berlin 🦀"
        call("post unicode message", "Chat.post", lambda: chat.post(message=text))

        def find_text(result: Any) -> None:
            messages, _ = result
            require(any(m.msg == text for m in messages), "Sent text missing or changed during SDK round trip")
            for message in messages:
                if message.msg == text:
                    require(abs(message.tstamp_date.timestamp() - time.time()) < 120, "Message timestamp is not current (millisecond protocol expected)")

        messages_result = call("message round trip", "Chat.get_messages", chat.get_messages, find_text)
        message = next((m for m in (messages_result[0] if messages_result else []) if m.msg == text), None)
        if message:
            call("reply", "Chat.post", lambda: chat.post(message="Reply " + marker, reply_to=message))
            call("delete own message", "Chat.delete_message", lambda: chat.delete_message(message))
            call("deleted message retained", "Chat.get_messages", chat.get_messages,
                 lambda result: require(any(m.mid == message.mid and m.deleted for m in result[0]), "Deleted message was not returned with deleted flag"))
        else:
            runner.blocked("reply and deletion", ["Chat.delete_message"], "Sent message was not read back")
        call("deprecated feed alias", "Chat.pull_chat_feed", chat.pull_chat_feed)
        call("mark read", "Chat.mark_as_read", chat.mark_as_read,
             lambda us: require(own.uid in {u.uid for u in us}, "Reader list does not contain bot"))
        call("read receipts", "Chat.get_readers", chat.get_readers, is_list)
        call("rename chat", "Chat.update_title", lambda: chat.update_title(marker + " renamed"))
        call("restrict sending", "Chat.set_allow_sending", lambda: chat.set_allow_sending(False))
        call("restore sending", "Chat.set_allow_sending", lambda: chat.set_allow_sending(True))
        call("close chat", "Chat.close", chat.close)
        call("reopen chat", "Chat.reopen", chat.reopen)
        if other:
            call("add member", "Chat.add_members", lambda: chat.add_members(users=[other]))
            runner.case("member addition visible", ["Chat.get_user_members"], lambda: require(other.uid in {u.uid for u in chat.get_user_members()}, "Added member not visible"))
            call("add admin", "Chat.add_admin", lambda: chat.add_admin(other))
            runner.case("admin addition visible", ["Chat.get_admins"], lambda: require(other.uid in {u.uid for u in chat.get_admins()}, "Added admin not visible"))
            call("remove admin", "Chat.remove_admin", lambda: chat.remove_admin(other))
            call("remove member", "Chat.remove_members", lambda: chat.remove_members(users=[other]))
            runner.case("member removal visible", ["Chat.get_user_members"], lambda: require(other.uid not in {u.uid for u in chat.get_user_members()}, "Removed member remains visible"))
            call("create one to one", "Chats.create_one2one_chat", lambda: conn.chats.create_one2one_chat(other, message=marker))
        else:
            runner.blocked("membership and one to one", ["Chat.add_members", "Chat.remove_members", "Chat.add_admin", "Chat.remove_admin", "Chats.create_one2one_chat"], "Supply --other-user-id for a second isolated fixture user")

        if args.extended:
            with tempfile.TemporaryDirectory(prefix="hoowire-sdk-conformance-") as temporary:
                folder = Path(temporary)
                payload = ("SDK file attachment " + marker).encode()
                source = folder / "fixture.txt"
                source.write_bytes(payload)
                attachments = runner.case("attachment collection", ["Chat.new_attachments"], chat.new_attachments)
                call("file upload", "Attachments.add_file", lambda: attachments.add_file(str(source)))
                call("post file", "Chat.post", lambda: chat.post(attachments=attachments))
                file_messages = call("read file attachment", "Chat.get_messages", chat.get_messages)
                assets = [a for m in (file_messages[0] if file_messages else []) for a in (m.attachments or []) if hasattr(a, "file_info")]
                if assets:
                    call("download file", "AssetAttachment.load", assets[-1].load, lambda data: require(bytes(data) == payload, "Attachment bytes changed"))
                    destination = folder / "download.txt"
                    call("store file", "AssetAttachment.store", lambda: assets[-1].store(str(destination)))
                    runner.case("stored file contents", ["AssetAttachment.store"], lambda: require(destination.read_bytes() == payload, "Stored attachment changed"))
                    forwarded = chat.new_attachments()
                    call("forward file attachment", "Attachments.add_attachment", lambda: forwarded.add_attachment(assets[-1]))
                    call("post forwarded attachment", "Chat.post", lambda: chat.post(attachments=forwarded))
                else:
                    runner.blocked("download file", ["AssetAttachment.load", "AssetAttachment.store"], "Uploaded file attachment was not read back")
                attachments = chat.new_attachments()
                call("location attachment", "Attachments.add_location", lambda: attachments.add_location("Berlin", 52.52, 13.405))
                now = datetime.now(timezone.utc)
                call("calendar attachment", "Attachments.add_calendar", lambda: attachments.add_calendar(marker, now, now + timedelta(hours=1)))
                call("post structured attachments", "Chat.post", lambda: chat.post(attachments=attachments))
                poll = chat.new_attachments()
                call("create poll", "Attachments.add_poll", lambda: poll.add_poll(marker, ["Yes", "No"], duration=3600))
                call("post poll", "Chat.post", lambda: chat.post(attachments=poll))
                poll_messages = call("read poll attachment", "Chat.get_messages", chat.get_messages)
                polls = [a for m in (poll_messages[0] if poll_messages else []) for a in (m.attachments or []) if isinstance(a, PollAttachment)]
                if polls:
                    call("poll state", "PollAttachment.getPollData", polls[-1].getPollData,
                         lambda p: require(p.question == marker and {c.text for c in p.choices} == {"Yes", "No"}, "Poll question or choices did not persist"))
                else:
                    runner.blocked("poll state", ["PollAttachment.getPollData"], "Posted poll was not read back")

                fixture_dir = Path(__file__).with_name("fixtures")
                for kind, attachment_type, file_name in (
                    ("gallery", AttachmentType.GALLERY, "pixel.jpg"),
                    ("sound", AttachmentType.SOUND, "tone.wav"),
                    ("video", AttachmentType.VIDEO, "clip.mp4"),
                ):
                    media = chat.new_attachments()
                    fixture = fixture_dir / file_name
                    call(f"{kind} bytes upload", f"Attachments.add_{kind}",
                         lambda media=media, kind=kind, fixture=fixture: getattr(media, f"add_{kind}")(fixture.read_bytes()))
                    call(f"post {kind}", "Chat.post", lambda media=media: chat.post(attachments=media))
                    media_messages = call(f"read {kind}", "Chat.get_messages", chat.get_messages)
                    matches = [a for m in (media_messages[0] if media_messages else []) for a in (m.attachments or []) if a.type == attachment_type]
                    if matches:
                        expected = fixture.read_bytes()
                        call(f"download {kind}", "AssetAttachment.load", matches[-1].load,
                             lambda actual, expected=expected: require(bytes(actual) == expected, "Media bytes changed during SDK round trip"))
                    else:
                        runner.blocked(f"download {kind}", ["AssetAttachment.load"], "Posted media attachment was not read back")
                    legacy_media = chat.new_attachments()
                    call(f"{kind} legacy file upload", f"Attachments.add_{kind}_file",
                         lambda media=legacy_media, kind=kind, fixture=fixture: getattr(media, f"add_{kind}_file")(str(fixture)))

        if args.avatars:
            avatar = Path(__file__).with_name("fixtures") / "pixel.png"
            image = avatar.read_bytes()
            call("set bot avatar", "LoggedInUser.set_avatar", lambda: own.set_avatar(str(avatar)))
            call("bot avatar exists", "User.has_avatar", own.has_avatar, lambda result: require(result is True, "Uploaded avatar not visible"))
            call("load bot avatar", "User.load_avatar", own.load_avatar, lambda result: require(bytes(result) == image, "Bot avatar bytes changed"))
            call("remove bot avatar", "LoggedInUser.remove_avatar", own.remove_avatar)
            call("bot avatar removed", "User.has_avatar", own.has_avatar, lambda result: require(result is False, "Removed avatar remains visible"))
            avatar = Path(__file__).with_name("fixtures") / "pixel.jpg"
            image = avatar.read_bytes()
            call("set chat avatar", "Chat.set_avatar", lambda: chat.set_avatar(str(avatar)))
            call("load chat avatar", "Chat.load_avatar", chat.load_avatar, lambda result: require(bytes(result) == image, "Chat avatar bytes changed"))
            call("remove chat avatar", "Chat.remove_avatar", chat.remove_avatar)

        if args.reactions:
            alert_text = "Attention " + marker
            call("post attention message", "Chat.post", lambda: chat.post(message=alert_text, is_alert=True, alert_type=AlertType.ALERT))
            alert_messages = call("read attention message", "Chat.get_messages", chat.get_messages)
            alert = next((m for m in (alert_messages[0] if alert_messages else []) if m.msg == alert_text), None)
            if alert:
                reaction = call("send attention reaction", "Chat.send_am_reaction", lambda: chat.send_am_reaction(alert, ReactionType.OK),
                                lambda r: require(r.reaction_type == ReactionType.OK and r.mid == alert.mid and r.from_user is not None and r.from_user.uid == own.uid, "Created reaction type, message or sender mismatch"))
                call("read attention reactions", "Chat.get_am_reactions", lambda: chat.get_am_reactions(alert),
                     lambda rs: require(any(r.reaction_type == ReactionType.OK for r in rs), "Reaction missing from readback"))
                if reaction:
                    call("update attention reaction", "Chat.update_am_reaction", lambda: chat.update_am_reaction(reaction, ReactionType.SOS))
                    if runner.results[-1]["status"] == "passed":
                        call("updated attention reaction visible", "Chat.get_am_reactions", lambda: chat.get_am_reactions(alert),
                             lambda rs: require(any(r.reaction_type == ReactionType.SOS for r in rs), "Updated reaction missing"))
                    else:
                        if runner.results[-1].get("error") == "AttributeError: 'AMReaction' object has no attribute 'id'":
                            runner.results[-1]["known_reference_sdk_defect"] = "SDK 2.0.0 parses reaction_id but update_am_reaction accesses id before making its HTTP request"
                        runner.blocked("updated attention reaction visible", ["Chat.get_am_reactions"], "SDK reaction update did not complete")
            else:
                runner.blocked("attention reactions", ["Chat.send_am_reaction", "Chat.get_am_reactions", "Chat.update_am_reaction"], "Attention message was not read back")

        if args.live_location:
            sharing = chat.new_attachments()
            call("create live location", "Attachments.add_location_sharing", lambda: sharing.add_location_sharing(52.52, 13.405, LocationSharingDuration.DURATION_15_MIN))
            call("post live location", "Chat.post", lambda: chat.post(attachments=sharing))
            runner.case("active sharing chats", ["Chats.get_location_sharing_active_chats"],
                        lambda: require(chat.tid in {c.tid for c in conn.chats.get_location_sharing_active_chats()}, "SDK did not track the active sharing chat"))
            runner.case("location sharing active", ["Chats.has_location_sharing_active_chats"],
                        lambda: require(conn.chats.has_location_sharing_active_chats(), "SDK did not track active sharing"))
            call("active location count", "Chat.get_active_shared_locations", chat.get_active_shared_locations,
                 lambda count: require(isinstance(count, int) and count >= 1, "Expected an active shared location"))
            call("update live location", "Chats.update_shared_location", lambda: conn.chats.update_shared_location(52.521, 13.406))
            location_messages = call("read live location", "Chat.get_messages", chat.get_messages)
            locations = [a for m in (location_messages[0] if location_messages else []) for a in (m.attachments or []) if isinstance(a, LocationSharingAttachment)]
            if locations:
                call("live location state", "LocationSharingAttachment.get_updated_location", locations[-1].get_updated_location,
                     lambda update: require(abs(update.lat - 52.521) < 1e-6 and abs(update.lon - 13.406) < 1e-6 and update.is_active and abs(update.last_active.timestamp() - time.time()) < 120,
                                            "Live location coordinates, active flag or timestamp changed"))
            else:
                runner.blocked("live location state", ["LocationSharingAttachment.get_updated_location"], "Live location attachment was not read back")
            call("stop live location", "Chat.stop_sharing_location", chat.stop_sharing_location)
            call("no active locations after stop", "Chat.get_active_shared_locations", chat.get_active_shared_locations,
                 lambda count: require(count == 0, "Stopped shared location remains active"))
        call("leave chat", "Chat.leave", chat.leave)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default=os.environ.get("TW_API_HOST", "http://127.0.0.1:8080"))
    parser.add_argument("--appid", default=os.environ.get("TW_API_APPID"))
    parser.add_argument("--secret", default=os.environ.get("TW_API_SECRET"))
    parser.add_argument("--wheel", type=Path, help="Licensed SDK wheel; dependencies must already be installed")
    parser.add_argument("--other-user-id", default=os.environ.get("HOOWIRE_TEST_OTHER_USER_ID"))
    parser.add_argument("--extended", action="store_true", help="Exercise file, calendar, location and poll attachments")
    parser.add_argument("--avatars", action="store_true", help="Exercise bot and chat avatar upload, download and removal")
    parser.add_argument("--reactions", action="store_true", help="Exercise attention messages and reaction updates")
    parser.add_argument("--live-location", action="store_true", help="Exercise live location sharing, update, state and stop")
    parser.add_argument("--notifications", action="store_true", help="Exercise incoming callbacks using a second disposable SDK bot")
    parser.add_argument("--sender-appid", default=os.environ.get("HOOWIRE_TEST_SENDER_APPID"))
    parser.add_argument("--sender-secret", default=os.environ.get("HOOWIRE_TEST_SENDER_SECRET"))
    parser.add_argument("--sender-user-id", default=os.environ.get("HOOWIRE_TEST_SENDER_USER_ID"))
    parser.add_argument("--allow-remote", action="store_true", help="Allow a non-loopback, explicitly selected test server")
    parser.add_argument("--report", type=Path, help="Write the complete JSON result to this file")
    parser.add_argument("--server-source-sha256", default=os.environ.get("HOOWIRE_TEST_SERVER_SOURCE_SHA256"), help="Optional operator-supplied digest of the source used to build the test server")
    parser.add_argument("--require-complete", action="store_true", help="Fail if any inventoried SDK method is not exercised")
    args = parser.parse_args()
    if not args.appid or not args.secret:
        parser.error("Supply --appid/--secret or TW_API_APPID/TW_API_SECRET for isolated fixture credentials")
    if urlparse(args.host).hostname not in {"127.0.0.1", "localhost", "::1"} and not args.allow_remote:
        parser.error("The suite mutates test data. Select a loopback server, or explicitly pass --allow-remote")
    if args.wheel:
        if not args.wheel.is_file():
            parser.error("--wheel does not point to a file")
        sys.path.insert(0, str(args.wheel.resolve()))
    try:
        sdk_version = importlib.metadata.version("teamwire_api")
    except importlib.metadata.PackageNotFoundError:
        parser.error("Install the independently licensed Teamwire SDK wheel and its dependencies first")
    if sdk_version != "2.0.0":
        parser.error(f"This suite targets SDK 2.0.0; installed SDK is {sdk_version}")
    surface_path = Path(__file__).with_name("sdk_surface.json")
    surface = json.loads(surface_path.read_text())["methods"] if surface_path.exists() else []
    runner = Runner()
    run_sdk(args, runner)
    if args.notifications:
        from sdk_notifications import run_notification_cases, run_subscription_cases
        run_notification_cases(args, runner)
        run_subscription_cases(args, runner)
    report = runner.report(args.host, sdk_version, surface)
    if args.server_source_sha256:
        report["server_source_sha256"] = args.server_source_sha256
    serialized = json.dumps(report, indent=2, ensure_ascii=False) + "\n"
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(serialized)
    print(serialized)
    return int(report["counts"]["failed"] > 0 or (args.require_complete and bool(report["not_qualified"])))


if __name__ == "__main__":
    raise SystemExit(main())

"""Bounded callback scenarios using the official SDK without modifying it."""

from __future__ import annotations

import asyncio
from contextlib import suppress
from uuid import uuid4


class _ScenarioFinished(KeyboardInterrupt):
    """Stop the SDK's intentionally infinite notification loop after a test."""


def _close_loop(loop: asyncio.AbstractEventLoop) -> None:
    if loop.is_closed():
        return
    pending = asyncio.all_tasks(loop)
    for task in pending:
        task.cancel()
    if pending:
        loop.run_until_complete(asyncio.gather(*pending, return_exceptions=True))
    loop.run_until_complete(loop.shutdown_asyncgens())
    loop.close()


def run_notification_cases(args, runner) -> None:
    from teamwire_api import Connection, Events
    from teamwire_api.connection import ConnectionConfig

    methods = ["Connection.set_handler", "Connection.start_notifications", "Connection.run_task"]
    if not all((args.sender_appid, args.sender_secret, args.sender_user_id)):
        runner.blocked("incoming SDK notifications", methods,
                       "Supply disposable --sender-appid, --sender-secret and --sender-user-id")
        return

    for entrypoint in ("start_notifications", "run_task"):
        def scenario(entrypoint=entrypoint):
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            marker = f"HooWire SDK callback {uuid4()}"
            received = asyncio.Event()
            failures: list[Exception] = []
            matches: list[object] = []
            config = ConnectionConfig(default_timeout=3, long_timeout=5)

            def stop() -> None:
                raise _ScenarioFinished()

            def send(tid: str) -> None:
                sender_loop = asyncio.new_event_loop()
                asyncio.set_event_loop(sender_loop)
                try:
                    with Connection(args.sender_appid, args.sender_secret, args.host,
                                    config=config, loop=sender_loop) as sender:
                        chat = sender.exec_async(sender.chats.get_by_thread_id(tid))
                        if chat is None:
                            raise AssertionError("Sender could not access the fixture chat")
                        sender.exec_async(chat.post(message=marker))
                finally:
                    _close_loop(sender_loop)

            try:
                with Connection(args.appid, args.secret, args.host, config=config, loop=loop) as receiver:
                    sender_user = receiver.exec_async(receiver.users.get_user(args.sender_user_id))
                    chat = receiver.exec_async(receiver.chats.create_chat(
                        title=marker, users=[receiver.own_user, sender_user], admins=[receiver.own_user]))

                    def on_message(event, message) -> None:
                        if message.msg == marker:
                            matches.append(message)
                            received.set()

                    receiver.set_handler([Events.NEW_MESSAGE], on_message)

                    async def task(connection) -> None:
                        try:
                            await asyncio.sleep(0.3)
                            await asyncio.to_thread(send, chat.tid)
                            await asyncio.wait_for(received.wait(), timeout=6)
                        except Exception as exc:
                            failures.append(exc)
                        finally:
                            # A callback outside the task allows its coroutine to finish
                            # normally before interrupting the SDK's endless listener.
                            loop.call_soon(stop)

                    try:
                        getattr(receiver, entrypoint)(task)
                    except _ScenarioFinished:
                        pass
                    if failures:
                        raise failures[0]
                    if not matches:
                        raise AssertionError("SDK NEW_MESSAGE callback was not invoked")
                    message = matches[-1]
                    if message.chat.tid != chat.tid or message.from_user.uid != args.sender_user_id:
                        raise AssertionError("Callback chat or sender identity changed")
            finally:
                _close_loop(loop)

        runner.case(f"incoming message through {entrypoint}",
                    ["Connection.set_handler", f"Connection.{entrypoint}"], scenario)


def run_subscription_cases(args, runner) -> None:
    from teamwire_api import AlertType, Connection, LocationSharingDuration, ReactionType
    from teamwire_api.connection import ConnectionConfig

    for kind in ("live_location", "attention_reaction"):
        if kind == "live_location" and not args.live_location:
            continue
        if kind == "attention_reaction" and not args.reactions:
            continue

        def scenario(kind=kind):
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            config = ConnectionConfig(default_timeout=3, long_timeout=5)
            marker = f"HooWire SDK subscription {uuid4()}"
            try:
                with Connection(args.appid, args.secret, args.host, config=config, loop=loop) as conn:
                    own = conn.own_user
                    chat = conn.exec_async(conn.chats.create_chat(title=marker, users=[own], admins=[own]))
                    received = asyncio.Event()
                    updates: list[object] = []

                    if kind == "live_location":
                        attachments = chat.new_attachments()
                        conn.exec_async(attachments.add_location_sharing(52.52, 13.405, LocationSharingDuration.DURATION_15_MIN))
                        conn.exec_async(chat.post(attachments=attachments))
                        subscribe = chat.subscribe_live_location_updates
                        unsubscribe = chat.unsubscribe_location_updates

                        def callback(update) -> None:
                            if abs(update.lat - 52.522) < 1e-6 and abs(update.lon - 13.407) < 1e-6:
                                updates.append(update)
                                received.set()

                        async def trigger() -> None:
                            await conn.chats.update_shared_location(52.522, 13.407)
                    else:
                        conn.exec_async(chat.post(message=marker, is_alert=True, alert_type=AlertType.ALERT))
                        messages, _ = conn.exec_async(chat.get_messages())
                        message = next(m for m in messages if m.msg == marker)
                        subscribe = conn.chats.subscribe_am_reaction_updates
                        unsubscribe = conn.chats.unsubscribe_am_reaction_updates

                        def callback(update) -> None:
                            if update.mid == message.mid and update.reaction_type == ReactionType.OK:
                                updates.append(update)
                                received.set()

                        async def trigger() -> None:
                            await chat.send_am_reaction(message, ReactionType.OK)

                    async def listen() -> None:
                        listener = asyncio.create_task(subscribe(callback))
                        try:
                            await asyncio.sleep(0.4)
                            await trigger()
                            await asyncio.wait_for(received.wait(), timeout=6)
                            update = updates[-1]
                            user = update.user if kind == "live_location" else update.from_user
                            if user is None or user.uid != own.uid:
                                raise AssertionError("Subscription callback sender identity changed")
                            if update.chat.tid != chat.tid:
                                raise AssertionError("Subscription callback chat identity changed")
                        finally:
                            await unsubscribe()
                            listener.cancel()
                            with suppress(asyncio.CancelledError):
                                await listener

                    conn.exec_async(listen())
            finally:
                _close_loop(loop)

        methods = (["Chat.subscribe_live_location_updates", "Chat.unsubscribe_location_updates"]
                   if kind == "live_location" else
                   ["Chats.subscribe_am_reaction_updates", "Chats.unsubscribe_am_reaction_updates"])
        runner.case(f"{kind} subscription callback and unsubscribe", methods, scenario)

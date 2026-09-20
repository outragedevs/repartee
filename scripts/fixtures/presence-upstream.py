import argparse
import asyncio
import json
from pathlib import Path


class PresenceServer:
    def __init__(self, events, setname=False, monitor=False, monitor_unavailable=False, invites=False, names=False, redaction=False):
        self.redaction = redaction
        self.setname = setname
        self.invites = invites
        self.names = names
        self.monitor = monitor
        self.monitor_unavailable = monitor_unavailable
        self.events = events
        self.next_connection = 0

    def record(self, connection, nick, away):
        with self.events.open('a') as output:
            output.write(json.dumps({'connection': connection, 'nick': nick, 'away': away}) + '\n')

    async def client(self, reader, writer):
        self.next_connection += 1
        connection = self.next_connection
        nick = '*'
        have_user = False
        negotiating = False
        registered = False
        monitored = set()
        monitor_available = self.monitor and not self.monitor_unavailable
        capabilities = {"setname"} if self.setname else set()
        if self.invites:
            capabilities.add("invite-notify")
        if self.redaction:
            capabilities.update(["draft/message-redaction", "message-tags", "echo-message"])
        if self.names:
            capabilities.update(["labeled-response", "message-tags", "batch", "echo-message"])
        if self.monitor:
            capabilities.update(["account-notify", "away-notify", "chghost", "setname", "extended-monitor"])

        def send(line):
            writer.write((line + '\r\n').encode())

        try:
            while raw := await reader.readline():
                line = raw.decode().rstrip('\r\n')
                request_label = None
                if line.startswith('@'):
                    tags, line = line.split(' ', 1)
                    request_label = dict(tag.partition('=')[::2] for tag in tags[1:].split(';')).get('label')
                head, separator, trailing = line.partition(' :')
                parts = head.split()
                if separator:
                    parts.append(trailing)
                if not parts:
                    continue
                command, *params = parts
                command = command.upper()
                if command == 'CAP' and params:
                    if params[0].upper() == 'LS':
                        negotiating = True
                        send(f':fixture.local CAP {nick} LS :{" ".join(sorted(capabilities))}')
                    elif params[0].upper() == 'REQ':
                        reply = 'ACK' if set(params[-1].split()) <= capabilities else 'NAK'
                        send(f':fixture.local CAP {nick} {reply} :{params[-1]}')
                    elif params[0].upper() == 'END':
                        negotiating = False
                elif command == 'NICK' and params:
                    nick = params[0]
                elif command == 'USER':
                    have_user = True
                elif command == 'PING':
                    send(f':fixture.local PONG fixture.local :{params[-1]}')
                elif command == 'PRIVMSG' and registered and self.redaction and params == ['FixtureControl', 'redaction-message']:
                    send('@msgid=upstream-redaction :Alice!peer@fixture.local PRIVMSG #redaction :fixture secret body')
                elif command == 'REDACT' and registered and self.redaction and len(params) >= 2:
                    with self.events.open('a') as output:
                        output.write(json.dumps({'redact': params}) + '\n')
                    if len(params) > 2 and params[2] == 'denied':
                        label = f'@label={request_label} ' if request_label else ''
                        send(f'{label}:fixture.local FAIL REDACT REDACT_FORBIDDEN {params[0]} {params[1]} :fixture denied deletion')
                    else:
                        reason = f' :{params[2]}' if len(params) > 2 else ''
                        send(f':{nick}!fixture@localhost REDACT {params[0]} {params[1]}{reason}')
                elif command == 'SETNAME' and registered and self.setname and params:
                    with self.events.open('a') as output:
                        output.write(json.dumps({'connection': connection, 'realname': params[0]}) + '\n')
                    send(f':{nick}!fixture@localhost SETNAME :{params[0]}')
                elif command == 'WHOIS' and registered and self.names and params:
                    target = params[-1]
                    prefix = ''
                    if request_label:
                        send(f'@label={request_label} :fixture.local BATCH +whois-reply labeled-response')
                        prefix = '@batch=whois-reply '
                    send(f'{prefix}:fixture.local 311 {nick} {target} user fixture.local * :fixture-labeled-whois')
                    send(f'{prefix}:fixture.local 318 {nick} {target} :End of WHOIS')
                    if request_label:
                        send(':fixture.local BATCH -whois-reply')
                elif command == 'PRIVMSG' and registered and (self.monitor or self.invites) and len(params) == 2 and params[0].lower() == 'fixturecontrol':
                    with self.events.open('a') as output:
                        output.write(json.dumps({'control': params[1]}) + '\n')
                    if params[1] == 'own-invite':
                        send(f':Inviter!user@fixture.local INVITE {nick} :#100%N')
                    elif params[1] == 'peer-invite':
                        send(':Inviter!user@fixture.local INVITE Other :#fixture')
                    elif params[1] == 'monitor-off':
                        monitor_available = False
                        send(f':fixture.local 005 {nick} -MONITOR :supported')
                    elif params[1] == 'monitor-on':
                        monitor_available = True
                        send(f':fixture.local 005 {nick} MONITOR=100 :supported')
                    elif params[1] == 'account-off':
                        capabilities.discard('account-notify')
                        send(f':fixture.local CAP {nick} DEL :account-notify')
                    elif params[1] == 'account-change':
                        send(':Alice!changed@changed.example ACCOUNT restored-account')
                    elif params[1] == 'account-on':
                        capabilities.add('account-notify')
                        send(f':fixture.local CAP {nick} NEW :account-notify')
                    send(f':FixtureControl!control@fixture.local NOTICE {nick} :control-complete {params[1]}')
                elif command == 'MONITOR' and registered and self.monitor and params:
                    if not monitor_available:
                        send(f':fixture.local 421 {nick} MONITOR :Unknown command')
                        await writer.drain()
                        continue
                    operation = params[0].upper()
                    names = params[1].split(',') if len(params) > 1 else []
                    if operation == '+':
                        rejected = [name for name in names if name.lower() == 'rejected']
                        for name in rejected:
                            send(f':fixture.local 734 {nick} 2 {name} :Monitor list is full')
                        names = [name for name in names if name not in rejected]
                        monitored.update(name.lower() for name in names)
                    elif operation == '-':
                        monitored.difference_update(name.lower() for name in names)
                    elif operation == 'C':
                        monitored.clear()
                    elif operation == 'L':
                        if monitored:
                            send(f':fixture.local 732 {nick} :{",".join(sorted(monitored))}')
                        send(f':fixture.local 733 {nick} :End of MONITOR list')
                    if operation in ['+', 'S']:
                        for name in (names if operation == '+' else sorted(monitored)):
                            online = name.lower() == 'alice'
                            mask = f'{name}!peer@fixture.local' if online else name
                            send(f':fixture.local {730 if online else 731} {nick} :{mask}')
                            if online:
                                send(f':{name}!peer@fixture.local ACCOUNT alice-account')
                                send(f':{name}!peer@fixture.local AWAY :Away fixture')
                                send(f':{name}!peer@fixture.local CHGHOST changed changed.example')
                                send(f':{name}!changed@changed.example SETNAME :Alice Fixture')
                elif command == 'AWAY' and registered:
                    away = params[0] if params else None
                    self.record(connection, nick, away)
                    numeric = '306' if away is not None else '305'
                    send(f':fixture.local {numeric} {nick} :Away state updated')
                elif command == 'JOIN' and registered and params:
                    for channel in params[0].split(','):
                        send(f':{nick}!fixture@localhost JOIN {channel}')
                        send(f':fixture.local 353 {nick} = {channel} :{nick}{" @Alice Bob" if self.names else ""}')
                        send(f':fixture.local 366 {nick} {channel} :End of NAMES')
                elif command == 'NAMES' and registered and self.names and params:
                    for channel in params[0].split(','):
                        send(f':fixture.local 353 {nick} = {channel} :{nick} @Alice Bob')
                        send(f':fixture.local 366 {nick} {channel} :End of NAMES')
                elif command == 'QUIT':
                    break
                if nick != '*' and have_user and not negotiating and not registered:
                    registered = True
                    self.record(connection, nick, None)
                    send(f':fixture.local 001 {nick} :Welcome to the disposable presence fixture')
                    send(f':fixture.local 005 {nick} CASEMAPPING=ascii CHANTYPES=# PREFIX=(ov)@+ {"MONITOR=100" if monitor_available else ""} :supported')
                    send(f':fixture.local 376 {nick} :End of MOTD')
                await writer.drain()
        finally:
            writer.close()
            await writer.wait_closed()


async def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('ready', type=Path)
    parser.add_argument('events', type=Path)
    parser.add_argument('--setname', action='store_true')
    parser.add_argument('--monitor', action='store_true')
    parser.add_argument('--monitor-unavailable', action='store_true')
    parser.add_argument('--invites', action='store_true')
    parser.add_argument('--names', action='store_true')
    parser.add_argument('--redaction', action='store_true')
    args = parser.parse_args()
    fixture = PresenceServer(args.events, args.setname, args.monitor, args.monitor_unavailable, args.invites, args.names, args.redaction)
    server = await asyncio.start_server(fixture.client, '127.0.0.1', 0)
    args.ready.write_text(json.dumps({'port': server.sockets[0].getsockname()[1]}))
    async with server:
        await server.serve_forever()


if __name__ == '__main__':
    asyncio.run(main())

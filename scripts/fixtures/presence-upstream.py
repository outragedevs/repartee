import argparse
import asyncio
import json
from pathlib import Path


class PresenceServer:
    def __init__(self, events):
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

        def send(line):
            writer.write((line + '\r\n').encode())

        try:
            while raw := await reader.readline():
                line = raw.decode().rstrip('\r\n')
                if line.startswith('@'):
                    line = line.split(' ', 1)[1]
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
                        send(f':fixture.local CAP {nick} LS :')
                    elif params[0].upper() == 'REQ':
                        send(f':fixture.local CAP {nick} NAK :{params[-1]}')
                    elif params[0].upper() == 'END':
                        negotiating = False
                elif command == 'NICK' and params:
                    nick = params[0]
                elif command == 'USER':
                    have_user = True
                elif command == 'PING':
                    send(f':fixture.local PONG fixture.local :{params[-1]}')
                elif command == 'AWAY' and registered:
                    away = params[0] if params else None
                    self.record(connection, nick, away)
                    numeric = '306' if away is not None else '305'
                    send(f':fixture.local {numeric} {nick} :Away state updated')
                elif command == 'JOIN' and registered and params:
                    for channel in params[0].split(','):
                        send(f':{nick}!fixture@localhost JOIN {channel}')
                        send(f':fixture.local 353 {nick} = {channel} :{nick}')
                        send(f':fixture.local 366 {nick} {channel} :End of NAMES')
                elif command == 'QUIT':
                    break
                if nick != '*' and have_user and not negotiating and not registered:
                    registered = True
                    self.record(connection, nick, None)
                    send(f':fixture.local 001 {nick} :Welcome to the disposable presence fixture')
                    send(f':fixture.local 005 {nick} CASEMAPPING=ascii CHANTYPES=# PREFIX=(ov)@+ :supported')
                    send(f':fixture.local 376 {nick} :End of MOTD')
                await writer.drain()
        finally:
            writer.close()
            await writer.wait_closed()


async def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('ready', type=Path)
    parser.add_argument('events', type=Path)
    args = parser.parse_args()
    fixture = PresenceServer(args.events)
    server = await asyncio.start_server(fixture.client, '127.0.0.1', 0)
    args.ready.write_text(json.dumps({'port': server.sockets[0].getsockname()[1]}))
    async with server:
        await server.serve_forever()


if __name__ == '__main__':
    asyncio.run(main())

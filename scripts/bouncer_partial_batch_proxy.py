"""Forward a real BETWEEN response prefix, then cut before its batch closes."""

from bouncer_fault_proxy import FaultProxy
from probe_bouncer_history_ranges import parse


class PartialBatchProxy(FaultProxy):
    def __init__(self, port, stall=False):
        super().__init__(port)
        self.armed = False
        self.requested = False
        self.batch = None
        self.rows = 0
        self.faulted = False
        self.buffers = {}
        self.stall = stall
        self.held = bytearray()
        self.held_destination = None
        self.released = False

    def extra_control(self, path):
        with self.lock:
            if path == '/arm':
                if self.armed or self.faulted:
                    return {'armed': False}
                self.armed = True
                return {'armed': True}
            if path == '/release' and self.held_destination is not None:
                size = len(self.held)
                self.held_destination.sendall(self.held)
                self.held.clear()
                self.held_destination = None
                self.released = True
                return {'released_bytes': size}
            if path == '/status':
                return {'requested': self.requested, 'batch_opened': self.batch is not None,
                        'forwarded_rows': self.rows, 'faulted': self.faulted,
                        'held_bytes': len(self.held), 'released': self.released}
        return None

    def forward(self, pair, source, data):
        with self.lock:
            armed = self.armed
            if source is pair[1] and self.held_destination is not None:
                self.held.extend(data)
                return True
            if self.faulted:
                return super().forward(pair, source, data)
        key = (pair, source)
        buffered = self.buffers.get(key, b'') + data
        destination = pair[1] if source is pair[0] else pair[0]
        while b'\r\n' in buffered:
            line, buffered = buffered.split(b'\r\n', 1)
            tags, command, params = parse(line.decode('utf-8'))
            if source is pair[0]:
                if armed and command == 'CHATHISTORY' and params[:2] == ['BETWEEN', 'history-peer']:
                    with self.lock:
                        self.requested = True
            elif armed and self.requested:
                if command == 'BATCH' and params[0].startswith('+') and params[1:] == ['chathistory', 'history-peer']:
                    with self.lock:
                        self.batch = params[0][1:]
                elif self.batch is not None and tags.get('batch') == self.batch and command == 'PRIVMSG':
                    destination.sendall(line + b'\r\n')
                    with self.lock:
                        self.rows += 1
                        self.faulted = True
                        self.armed = False
                        if self.stall:
                            self.held_destination = destination
                            self.held.extend(buffered)
                            self.buffers.clear()
                            return True
                    self.buffers.clear()
                    self.cut()
                    return False
            destination.sendall(line + b'\r\n')
        self.buffers[key] = buffered
        return True

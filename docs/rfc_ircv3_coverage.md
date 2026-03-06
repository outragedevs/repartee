# RFC 2812 / IRCv3 Coverage Roadmap

Track implementation status of IRC protocol features in rustirc.

**Legend:** Done | In Progress | Not Started

---

## RFC 2812 — Core Protocol

### Connection Registration (3.1)

| Feature | Status | Notes |
|---------|--------|-------|
| PASS | Done | Server password sent before registration |
| NICK | Done | Nickname setting + ERR_NICKNAMEINUSE retry |
| USER | Done | Username/realname registration |
| OPER | Not Started | IRC operator login |
| QUIT | Done | Disconnect with message |
| USER MODE | Done | +i, +w, etc. |

### Channel Operations (3.2)

| Feature | Status | Notes |
|---------|--------|-------|
| JOIN | Done | Multiple channels, keys |
| PART | Done | Leave with reason |
| CHANNEL MODE | Done | +beiklmnostRIv and prefix modes |
| TOPIC | Done | Get/set, RPL_TOPIC/RPL_TOPICWHOTIME |
| NAMES | Done | RPL_NAMREPLY with prefix parsing |
| LIST | Done | Channel listing |
| INVITE | Done | Invite user + notification |
| KICK | Done | Kick with reason |

### Messaging (3.3)

| Feature | Status | Notes |
|---------|--------|-------|
| PRIVMSG | Done | Channel + private, CTCP ACTION |
| NOTICE | Done | Server + user notices |

### Server Queries (3.4)

| Feature | Status | Notes |
|---------|--------|-------|
| MOTD | Done | Full MOTD display |
| LUSERS | Not Started | Server user/channel stats |
| VERSION (server) | Not Started | Server version query |
| STATS | Not Started | Server statistics |
| LINKS | Not Started | Server links |
| TIME | Not Started | Server time query |
| ADMIN | Not Started | Admin info |
| INFO | Not Started | Server info |

### User Queries (3.6)

| Feature | Status | Notes |
|---------|--------|-------|
| WHO | Done | Basic WHO reply parsing |
| WHOIS | Done | Full multi-line WHOIS display |
| WHOWAS | Done | Offline user lookup |

### Miscellaneous (3.7)

| Feature | Status | Notes |
|---------|--------|-------|
| PING/PONG | Done | Handled by irc crate |
| AWAY | Done | Set/unset + RPL_AWAY display |
| WALLOPS | Done | Wall message display |
| ERROR | Not Started | Connection termination handling |
| USERHOST | Not Started | Quick user lookup |
| ISON | Not Started | Online check (superseded by MONITOR) |

### CTCP

| Feature | Status | Notes |
|---------|--------|-------|
| ACTION | Done | Send + receive |
| VERSION | Done | Auto-response via irc crate |
| PING | Done | Auto-response via irc crate |
| TIME | Done | Auto-response via irc crate |
| FINGER | Done | Auto-response via irc crate |
| SOURCE | Done | Auto-response via irc crate |

### ISUPPORT (005)

| Feature | Status | Notes |
|---------|--------|-------|
| Token collection | Done | Raw tokens stored on Connection |
| Structured parsing | Done | `Isupport` struct parses PREFIX, CHANMODES, NETWORK, STATUSMSG, WHOX, EXTBAN, CASEMAPPING, lengths |
| Behavior adaptation | Done | `isupport_parsed` on Connection, updated on each RPL_ISUPPORT, NETWORK drives label |

---

## IRCv3 — Capability Negotiation

| Feature | Status | Notes |
|---------|--------|-------|
| CAP LS 302 | Done | Multiline parsing, field3/field4 handling |
| CAP REQ/ACK/NAK | Done | Request + detect acceptance |
| CAP END | Done | Properly closes negotiation |
| CAP NEW/DEL (cap-notify) | Not Started | Runtime capability changes |
| Capability state machine | Done | Extensible `negotiate_caps()` framework: `ServerCaps` struct, requests all `DESIRED_CAPS`, stores enabled caps on Connection |

---

## IRCv3 — Must Have (Tier 1)

| Capability | Status | Spec | Notes |
|------------|--------|------|-------|
| `multi-prefix` | Not Started | 3.1 | Multiple mode prefixes per user in NAMES/WHO |
| `extended-join` | Not Started | 3.1 | JOIN includes account + realname |
| `server-time` | Not Started | 3.2 | Server-provided timestamps on messages |
| `account-tag` | Not Started | 3.2 | User account in message tags |
| `cap-notify` | Not Started | 3.2 | Server notifies of cap changes (CAP NEW/DEL) |
| `away-notify` | Not Started | 3.1 | Real-time AWAY status changes |
| `account-notify` | Not Started | 3.1 | Real-time account login/logout (ACCOUNT cmd) |
| `chghost` | Not Started | 3.2 | Host/ident change notifications |
| SASL EXTERNAL | Not Started | 3.1 | CertFP-based authentication |
| SASL SCRAM-SHA-256 | Not Started | 3.1 | Challenge-response SASL mechanism |
| SASL mechanism selection | Not Started | — | Pick best mechanism from server's advertised list |

---

## IRCv3 — High Value (Tier 2)

| Capability | Status | Spec | Notes |
|------------|--------|------|-------|
| `echo-message` | Not Started | 3.2 | Server echoes own messages back |
| `invite-notify` | Not Started | 3.2 | Channel members see invites |
| `batch` | Not Started | 3.2 | NETSPLIT/NETJOIN batching, generic batch support |
| `userhost-in-names` | Not Started | 3.2 | Full user@host in NAMES reply |
| `message-tags` | Done | 3.2 | Plumbing: tags extracted from IRC messages, stored in buffer `Message` and DB |

---

## IRCv3 — Nice to Have (Tier 3)

| Capability | Status | Spec | Notes |
|------------|--------|------|-------|
| `monitor` | Not Started | 3.2 | Nick online/offline tracking |
| `labeled-response` | Not Started | 3.2 | Match responses to requests |
| `msgid` | Not Started | — | Message deduplication IDs |
| `reply` | Not Started | — | Message threading via +draft/reply |
| `setname` | Not Started | — | Real-time realname changes |

---

## IRCv3 — Skipped

| Capability | Reason |
|------------|--------|
| `metadata` / `metadata-notify` | Rarely deployed |
| `sts` (Strict Transport Security) | Out of scope for now |
| `zcrypt` | Niche, rarely used |

---

## Custom Extensions

| Feature | Status | Source | Notes |
|---------|--------|--------|-------|
| WHOX | Not Started | [contempt-chat/ircd](https://github.com/contempt-chat/ircd/blob/master/doc/whox.md) | Extended WHO with field selectors (%tcuihsnfdlaor), token matching, account field. Auto-detect via ISUPPORT WHOX token. |
| Extban | Not Started | [contempt-chat/ircd](https://github.com/contempt-chat/ircd/blob/master/doc/extban.md) | `$a:account!user@host` — account-based bans/exempts/invites. Display + compose support. |

---

## Implementation Notes

- Each completed capability must be marked **Done** in this file
- ISUPPORT structured parsing is foundational — many caps depend on it
- Cap negotiation framework must be extensible for future tier 3 additions
- All new protocol handling must have tests
- Message tags must propagate to scripting API and storage layer

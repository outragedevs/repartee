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
| NAMES | Done | RPL_NAMREPLY with prefix parsing, multi-prefix, userhost-in-names |
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
| CAP NEW/DEL (cap-notify) | Done | Runtime capability changes: NEW triggers CAP REQ for desired caps, DEL removes from enabled_caps, ACK/NAK handled |
| Capability state machine | Done | Extensible `negotiate_caps()` framework: `ServerCaps` struct, requests all `DESIRED_CAPS`, stores enabled caps on Connection |

---

## IRCv3 — Must Have (Tier 1)

| Capability | Status | Spec | Notes |
|------------|--------|------|-------|
| `multi-prefix` | Done | 3.1 | All mode prefixes per user in NAMES, dynamic via ISUPPORT PREFIX |
| `extended-join` | Done | 3.1 | JOIN includes account + realname; account stored on `NickEntry` |
| `server-time` | Done | 3.2 | `@time` tag used as message timestamp; fallback to `Utc::now()` for missing/malformed tags |
| `account-tag` | Done | 3.2 | User account in message tags; supplementary update on `NickEntry` via PRIVMSG tags |
| `cap-notify` | Done | 3.2 | Server notifies of cap changes; CAP NEW auto-requests desired caps, CAP DEL removes, ACK/NAK logged |
| `away-notify` | Done | 3.1 | Real-time AWAY status changes; silently updates `NickEntry.away` across all shared buffers |
| `account-notify` | Done | 3.1 | ACCOUNT command: login/logout updates `NickEntry.account` across all shared buffers |
| `chghost` | Done | 3.2 | Host/ident change notifications; updates `NickEntry.ident`/`host`, adds event message |
| SASL EXTERNAL | Not Started | 3.1 | CertFP-based authentication |
| SASL SCRAM-SHA-256 | Not Started | 3.1 | Challenge-response SASL mechanism |
| SASL mechanism selection | Not Started | — | Pick best mechanism from server's advertised list |

---

## IRCv3 — High Value (Tier 2)

| Capability | Status | Spec | Notes |
|------------|--------|------|-------|
| `echo-message` | Done | 3.2 | Server echoes own messages back; local echo suppressed when cap enabled, own PRIVMSG/NOTICE/ACTION routed to correct buffer |
| `invite-notify` | Done | 3.2 | Channel members see invites; third-party INVITE shown in channel buffer |
| `batch` | Done | 3.2 | `BatchTracker` per connection; NETSPLIT/NETJOIN produce summary messages; unknown batch types replay normally; wired in app event loop |
| `userhost-in-names` | Done | 3.2 | `nick!user@host` parsing in NAMES, stored on `NickEntry` |
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

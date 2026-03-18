#!/usr/bin/env bash
# Build the computing.dic / computing.aff Hunspell dictionary.
#
# Merges words from:
#   1. streetsidesoftware/cspell-dicts (software-terms)
#   2. smoeding/hunspell-jargon
#   3. LibreOffice technical.dic
#   4. psliwka/vim-dirtytalk (selected wordlists)
#   5. Hand-curated IRC/IRCnet vocabulary
#
# Output: computing.dic + computing.aff in the script's output directory.

set -euo pipefail

OUT_DIR="${1:-$(dirname "$0")/../dicts}"
mkdir -p "$OUT_DIR"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

echo "==> Working in $WORK"

# ── 1. cspell software-terms ────────────────────────────────────────────
CSPELL_BASE="https://raw.githubusercontent.com/streetsidesoftware/cspell-dicts/main/dictionaries/software-terms/src"
CSPELL_FILES=(
    software-terms.txt
    coding-terms.txt
    software-tools.txt
    computing-acronyms.txt
    network-protocols.txt
    cybersecurity-terms.txt
    software-services.txt
    coding-compound-terms.txt
    network-os.txt
)
echo "==> Fetching cspell software-terms..."
for f in "${CSPELL_FILES[@]}"; do
    curl -sL "$CSPELL_BASE/$f" >> "$WORK/cspell-raw.txt"
    echo "" >> "$WORK/cspell-raw.txt"
done

# ── 2. hunspell-jargon ──────────────────────────────────────────────────
echo "==> Fetching hunspell-jargon..."
curl -sL "https://raw.githubusercontent.com/smoeding/hunspell-jargon/master/jargon.dic" > "$WORK/jargon-raw.txt"

# ── 3. LibreOffice technical.dic ─────────────────────────────────────────
echo "==> Fetching LibreOffice technical.dic..."
curl -sL "https://raw.githubusercontent.com/LibreOffice/core/master/extras/source/wordbook/technical.dic" > "$WORK/libreoffice-raw.txt"

# ── 4. vim-dirtytalk (selected) ─────────────────────────────────────────
DIRTYTALK_BASE="https://raw.githubusercontent.com/psliwka/vim-dirtytalk/master/wordlists"
DIRTYTALK_FILES=(acronyms.words unix.words algorithms.words docker.words git.words kubernetes.words)
echo "==> Fetching vim-dirtytalk wordlists..."
for f in "${DIRTYTALK_FILES[@]}"; do
    curl -sL "$DIRTYTALK_BASE/$f" >> "$WORK/dirtytalk-raw.txt"
    echo "" >> "$WORK/dirtytalk-raw.txt"
done

# ── 5. IRC / IRCnet vocabulary ──────────────────────────────────────────
echo "==> Adding IRC vocabulary..."
cat > "$WORK/irc-words.txt" << 'IRCEOF'
# IRC Protocol
PRIVMSG
NOTICE
CTCP
DCC
WHOIS
WHOWAS
OPER
MOTD
LUSERS
WALLOPS
USERHOST
ISON
AWAY
REHASH
SQUIT
NAMESX
UHNAMES
PROTOCTL
CAPAB
ISUPPORT
KNOCK
CNOTICE
CPRIVMSG
ENCAP
CHGHOST
SETHOST
SETNAME
ACCOUNT
AUTHENTICATE
BATCH
TAGMSG
METADATA
MONITOR
IRCX
STARTTLS

# IRCv3 capabilities
multi-prefix
extended-join
server-time
account-tag
cap-notify
away-notify
account-notify
chghost
echo-message
invite-notify
userhost-in-names
message-tags
sasl
labeled-response
msgid
chathistory
znc.in/server-time-iso
znc.in/playback
znc.in/self-message

# IRC networks
IRCnet
EFnet
Undernet
DALnet
QuakeNet
LiberaChat
Libera
OFTC
Rizon
Freenode
GameSurge
SwiftIRC
Snoonet
hackint
PIRC
IRCHighway
Esper
SynIRC
GeekShed
slashnet
BSDUnix
AfterNET
ChatNet
freenode
Azzurra
PTnet
GIMPnet
OSIRION
SpotChat
ICQ-Chat

# IRC clients
irssi
WeeChat
weechat
mIRC
mirc
HexChat
hexchat
Konversation
Quassel
Textual
LimeChat
Colloquy
Pidgin
XChat
xchat
KiwiIRC
kiwiirc
TheLounge
thelounge
AdiIRC
KVIrc
kvirc
BitchX
bitchx
ircII
EPIC
epic5
Smuxi
Goguma
Halloy
halloy
Circl
Revolution
Repartee
repartee
kokoirc
erssi
ERC
erc

# IRC bouncers / proxies
ZNC
znc
soju
pounce
shroud
BNC
bnc
IRCCloud
irccloud
Palaver

# IRC servers / daemons
ircd
IRCd
InspIRCd
inspircd
UnrealIRCd
unrealircd
ngircd
charybdis
solanum
ratbox
bahamut
Hybrid
hybrid-ircd
ircu
ircd-seven
atheme-ircd
Oragono
oragono
Ergo
ergo

# IRC services
Anope
anope
Atheme
atheme
ChanServ
chanserv
NickServ
nickserv
MemoServ
memoserv
BotServ
botserv
HostServ
hostserv
OperServ
operserv
SaslServ
GameServ
ChanFix
chanfix
X3
GNUWorld

# IRC bots
eggdrop
Eggdrop
supybot
Supybot
limnoria
Limnoria
sopel
Sopel
goat
energymech
Energymech

# IRC concepts
chanop
chanops
halfop
halfops
IRCop
IRCops
ircop
ircops
deop
deops
devoice
kickban
akick
autoop
autovoice
k-line
kline
g-line
gline
z-line
zline
shun
netsplit
netjoin
netmerge
desync
desynced
vhost
cloaking
cloak
cloaked
ident
identd
oidentd
hostmask
usermask
banmask
extban
extbans
autoconnect
autojoin
scrollback
backlog
SASL
sasl
ctcp
dcc
whois
whowas
nickserv
chanserv
operserv
usermode
chanmode
servermode
oline
olines

# IRC modes / prefixes
+o
+v
+h
+a
+q

# Common IRC terminology
msg
privmsg
oper
opers
ops
op
voiced
halfvoiced
voiced
devoiced
founder
founder-mode
ban
bans
unban
except
invex
MOTD
motd
lusers
admin
rehash
squit
wallop
wallops
logon
signon
signoff
takeover
flood
flooding
antiflood
throttle
throttling
ratelimit
lag
lagged
lagcheck
ping
pong
ctcpversion
ctcpreply
channel
channels
query
queries
buffer
buffers

# CTCP types
VERSION
FINGER
SOURCE
USERINFO
CLIENTINFO
ERRMSG
TIME
PING
ACTION
DCC
SED

# DCC types
DCC-CHAT
DCC-SEND
DCC-RESUME
DCC-ACCEPT
XDCC
TDCC
fserve

# Common tech terms used in IRC
SSL
TLS
IPv4
IPv6
TCP
UDP
hostname
hostnames
dns
DNS
reverse-dns
rdns
RDNS
localhost
regex
regexp
wildcard
wildcards
glob
globbing
webhook
webhooks
config
configs
plugin
plugins
addon
addons
charset
charsets
utf-8
UTF-8
unicode
Unicode
ASCII
ascii
ANSI
ansi
emoji
emojis
encodings
codepage

# Additional IRC-related terms
highlights
highlight
hilight
hilights
beep
bell
notify
notification
notifications
windowlist
nicklist
statusbar
inputbar
topicbar
scrollbar
sidebar
split
splits
layout
layouts
colorscheme
colorschemes
theme
themes
timestamp
timestamps

# IRCnet specific
O-line
C/N-lines
I-line
K-line
L-line
jupes
juped
reop
reopping
IRCEOF

# ── Merge & deduplicate ─────────────────────────────────────────────────
echo "==> Merging and deduplicating..."
cat "$WORK"/cspell-raw.txt "$WORK"/jargon-raw.txt "$WORK"/libreoffice-raw.txt \
    "$WORK"/dirtytalk-raw.txt "$WORK"/irc-words.txt \
    | sed 's/\r$//' \
    | sed 's/ *#.*//' \
    | grep -v '^#' \
    | grep -v '^---' \
    | grep -v '^OOoUserDict' \
    | grep -v '^lang:' \
    | grep -v '^type:' \
    | sed '/^[0-9]*$/d' \
    | sed '/^$/d' \
    | sed 's/^[[:space:]]*//' \
    | sed 's/[[:space:]]*$//' \
    | grep -v '^\*' \
    | grep -v '^\[' \
    | grep -v '^\]' \
    | grep -v '^(' \
    | grep -v '^α' \
    | grep -v '^[0-9a-f]\{4,\}$' \
    | grep -v '/' \
    | sort -u \
    > "$WORK/merged.txt"

TOTAL=$(wc -l < "$WORK/merged.txt" | tr -d ' ')
echo "==> Total unique words: $TOTAL"

# ── Write computing.aff ─────────────────────────────────────────────────
cat > "$OUT_DIR/computing.aff" << 'AFFEOF'
SET UTF-8
TRY esianrtolcdugmphbyfvkwzESIANRTOLCDUGMPHBYFVKWZ
NOSUGGEST !
AFFEOF

# ── Write computing.dic ─────────────────────────────────────────────────
echo "$TOTAL" > "$OUT_DIR/computing.dic"
cat "$WORK/merged.txt" >> "$OUT_DIR/computing.dic"

echo "==> Built $OUT_DIR/computing.dic ($TOTAL words)"
echo "==> Built $OUT_DIR/computing.aff"
echo "==> Done!"

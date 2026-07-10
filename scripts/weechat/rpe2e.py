# -*- coding: utf-8 -*-
#
# rpe2e.py — RPE2E v1.0 end-to-end encryption for WeeChat
#
# Copyright (c) 2026 repartee authors. MIT licensed.
#
# Wire-compatible with the native repartee implementation and the irssi
# rpe2e.pl script. See docs/plans/2026-04-10-e2e-encryption-architecture.md
# for the protocol specification.
#
# Dependencies:
#   pip install pynacl
#
# Install:
#   cp scripts/weechat/rpe2e.py ~/.weechat/python/autoload/
#   /python load rpe2e.py
#   /e2e fingerprint      # show your SAS
#   /e2e on               # enable on the current channel
#

from __future__ import annotations

import base64
import hashlib
import hmac as hmac_mod
import json
import os
import sqlite3
import struct
import time
import fnmatch
import collections
import traceback
from contextlib import contextmanager

try:
    import weechat
except ImportError:
    weechat = None

from nacl.signing import SigningKey, VerifyKey
from nacl.bindings import (
    crypto_aead_xchacha20poly1305_ietf_encrypt,
    crypto_aead_xchacha20poly1305_ietf_decrypt,
    crypto_aead_xchacha20poly1305_ietf_NPUBBYTES,
    crypto_aead_xchacha20poly1305_ietf_KEYBYTES,
    crypto_scalarmult,
    crypto_scalarmult_base,
    crypto_sign_BYTES,
    crypto_sign_ed25519_pk_to_curve25519,
    crypto_sign_ed25519_sk_to_curve25519,
)
from nacl.exceptions import BadSignatureError
from nacl.public import PrivateKey as X25519Priv, PublicKey as X25519Pub
from nacl.utils import random as nacl_random

SCRIPT_NAME = "rpe2e"
SCRIPT_AUTHOR = "repartee"
SCRIPT_VERSION = "0.2.0"
SCRIPT_LICENSE = "MIT"
SCRIPT_DESC = "RPE2E v1.0 end-to-end encryption (wire-compatible with repartee/irssi)"

PROTO = "RPE2E01"
WIRE_PREFIX = "+RPE2E01"
CTCP_TAG = "RPEE2E"
MAX_CHUNKS = 16
MAX_PT_PER_CHUNK = 180
TS_TOLERANCE = 300
KEYREQ_MIN_INTERVAL = 30
PENDING_KEYREQ_TTL = 120
HKDF_SALT = b"RPE2E01-WRAP"
NONCE_LEN = crypto_aead_xchacha20poly1305_ietf_NPUBBYTES
KEY_LEN = crypto_aead_xchacha20poly1305_ietf_KEYBYTES
CHANNEL_PREFIXES = ("#", "&", "!", "+")

INCOMING_MAX_PER_WINDOW = 3
INCOMING_WINDOW_SECS = 60
INCOMING_BACKOFF_SECS = 300

C_OK = weechat.color("green") if weechat else ""
C_ERR = weechat.color("red") if weechat else ""
C_WARN = weechat.color("yellow") if weechat else ""
C_INFO = weechat.color("cyan") if weechat else ""
C_RST = weechat.color("reset") if weechat else ""

DEBUG_LOG = os.path.expanduser("~/.weechat/rpe2e-debug.log")
DEBUG_ENABLED = os.environ.get("RPE2E_DEBUG") == "1"
DEBUG_BUFFER_ENABLED = os.environ.get("RPE2E_DEBUG_BUFFER") == "1"


def _dbg(msg: str) -> None:
    if not DEBUG_ENABLED:
        return
    try:
        with open(DEBUG_LOG, "a", encoding="utf-8") as f:
            f.write(f"{time.strftime('%Y-%m-%dT%H:%M:%S')} {msg}\n")
    except Exception:
        pass

BIP39_WORDS = [
    "abandon", "ability", "able", "about", "above", "absent", "absorb", "abstract",
    "absurd", "abuse", "access", "accident", "account", "accuse", "achieve", "acid",
    "acoustic", "acquire", "across", "act", "action", "actor", "actress", "actual",
    "adapt", "add", "addict", "address", "adjust", "admit", "adult", "advance",
    "advice", "aerobic", "affair", "afford", "afraid", "again", "age", "agent",
    "agree", "ahead", "aim", "air", "airport", "aisle", "alarm", "album",
    "alcohol", "alert", "alien", "all", "alley", "allow", "almost", "alone",
    "alpha", "already", "also", "alter", "always", "amateur", "amazing", "among",
    "amount", "amused", "analyst", "anchor", "ancient", "anger", "angle", "angry",
    "animal", "ankle", "announce", "annual", "another", "answer", "antenna", "antique",
    "anxiety", "any", "apart", "apology", "appear", "apple", "approve", "april",
    "arch", "arctic", "area", "arena", "argue", "arm", "armed", "armor",
    "army", "around", "arrange", "arrest", "arrive", "arrow", "art", "artefact",
    "artist", "artwork", "ask", "aspect", "assault", "asset", "assist", "assume",
    "asthma", "athlete", "atom", "attack", "attend", "attitude", "attract", "auction",
    "audit", "august", "aunt", "author", "auto", "autumn", "average", "avocado",
    "avoid", "awake", "aware", "away", "awesome", "awful", "awkward", "axis",
    "baby", "bachelor", "bacon", "badge", "bag", "balance", "balcony", "ball",
    "bamboo", "banana", "banner", "bar", "barely", "bargain", "barrel", "base",
    "basic", "basket", "battle", "beach", "bean", "beauty", "because", "become",
    "beef", "before", "begin", "behave", "behind", "believe", "below", "belt",
    "bench", "benefit", "best", "betray", "better", "between", "beyond", "bicycle",
    "bid", "bike", "bind", "biology", "bird", "birth", "bitter", "black",
    "blade", "blame", "blanket", "blast", "bleak", "bless", "blind", "blood",
    "blossom", "blouse", "blue", "blur", "blush", "board", "boat", "body",
    "boil", "bomb", "bone", "bonus", "book", "boost", "border", "boring",
    "borrow", "boss", "bottom", "bounce", "box", "boy", "bracket", "brain",
    "brand", "brass", "brave", "bread", "breeze", "brick", "bridge", "brief",
    "bright", "bring", "brisk", "broccoli", "broken", "bronze", "broom", "brother",
    "brown", "brush", "bubble", "buddy", "budget", "buffalo", "build", "bulb",
    "bulk", "bullet", "bundle", "bunker", "burden", "burger", "burst", "bus",
    "business", "busy", "butter", "buyer", "buzz", "cabbage", "cabin", "cable",
    "cactus", "cage", "cake", "call", "calm", "camera", "camp", "can",
    "canal", "cancel", "candy", "cannon", "canoe", "canvas", "canyon", "capable",
    "capital", "captain", "car", "carbon", "card", "cargo", "carpet", "carry",
    "cart", "case", "cash", "casino", "castle", "casual", "cat", "catalog",
    "catch", "category", "cattle", "caught", "cause", "caution", "cave", "ceiling",
    "celery", "cement", "census", "century", "cereal", "certain", "chair", "chalk",
    "champion", "change", "chaos", "chapter", "charge", "chase", "chat", "cheap",
    "check", "cheese", "chef", "cherry", "chest", "chicken", "chief", "child",
    "chimney", "choice", "choose", "chronic", "chuckle", "chunk", "churn", "cigar",
    "cinnamon", "circle", "citizen", "city", "civil", "claim", "clap", "clarify",
    "claw", "clay", "clean", "clerk", "clever", "click", "client", "cliff",
    "climb", "clinic", "clip", "clock", "clog", "close", "cloth", "cloud",
    "clown", "club", "clump", "cluster", "clutch", "coach", "coast", "coconut",
    "code", "coffee", "coil", "coin", "collect", "color", "column", "combine",
    "come", "comfort", "comic", "common", "company", "concert", "conduct", "confirm",
    "congress", "connect", "consider", "control", "convince", "cook", "cool", "copper",
    "copy", "coral", "core", "corn", "correct", "cost", "cotton", "couch",
    "country", "couple", "course", "cousin", "cover", "coyote", "crack", "cradle",
    "craft", "cram", "crane", "crash", "crater", "crawl", "crazy", "cream",
    "credit", "creek", "crew", "cricket", "crime", "crisp", "critic", "crop",
    "cross", "crouch", "crowd", "crucial", "cruel", "cruise", "crumble", "crunch",
    "crush", "cry", "crystal", "cube", "culture", "cup", "cupboard", "curious",
    "current", "curtain", "curve", "cushion", "custom", "cute", "cycle", "dad",
    "damage", "damp", "dance", "danger", "daring", "dash", "daughter", "dawn",
    "day", "deal", "debate", "debris", "decade", "december", "decide", "decline",
    "decorate", "decrease", "deer", "defense", "define", "defy", "degree", "delay",
    "deliver", "demand", "demise", "denial", "dentist", "deny", "depart", "depend",
    "deposit", "depth", "deputy", "derive", "describe", "desert", "design", "desk",
    "despair", "destroy", "detail", "detect", "develop", "device", "devote", "diagram",
    "dial", "diamond", "diary", "dice", "diesel", "diet", "differ", "digital",
    "dignity", "dilemma", "dinner", "dinosaur", "direct", "dirt", "disagree", "discover",
    "disease", "dish", "dismiss", "disorder", "display", "distance", "divert", "divide",
    "divorce", "dizzy", "doctor", "document", "dog", "doll", "dolphin", "domain",
    "donate", "donkey", "donor", "door", "dose", "double", "dove", "draft",
    "dragon", "drama", "drastic", "draw", "dream", "dress", "drift", "drill",
    "drink", "drip", "drive", "drop", "drum", "dry", "duck", "dumb",
    "dune", "during", "dust", "dutch", "duty", "dwarf", "dynamic", "eager",
    "eagle", "early", "earn", "earth", "easily", "east", "easy", "echo",
    "ecology", "economy", "edge", "edit", "educate", "effort", "egg", "eight",
    "either", "elbow", "elder", "electric", "elegant", "element", "elephant", "elevator",
    "elite", "else", "embark", "embody", "embrace", "emerge", "emotion", "employ",
    "empower", "empty", "enable", "enact", "end", "endless", "endorse", "enemy",
    "energy", "enforce", "engage", "engine", "enhance", "enjoy", "enlist", "enough",
    "enrich", "enroll", "ensure", "enter", "entire", "entry", "envelope", "episode",
    "equal", "equip", "era", "erase", "erode", "erosion", "error", "erupt",
    "escape", "essay", "essence", "estate", "eternal", "ethics", "evidence", "evil",
    "evoke", "evolve", "exact", "example", "excess", "exchange", "excite", "exclude",
    "excuse", "execute", "exercise", "exhaust", "exhibit", "exile", "exist", "exit",
    "exotic", "expand", "expect", "expire", "explain", "expose", "express", "extend",
    "extra", "eye", "eyebrow", "fabric", "face", "faculty", "fade", "faint",
    "faith", "fall", "false", "fame", "family", "famous", "fan", "fancy",
    "fantasy", "farm", "fashion", "fat", "fatal", "father", "fatigue", "fault",
    "favorite", "feature", "february", "federal", "fee", "feed", "feel", "female",
    "fence", "festival", "fetch", "fever", "few", "fiber", "fiction", "field",
    "figure", "file", "film", "filter", "final", "find", "fine", "finger",
    "finish", "fire", "firm", "first", "fiscal", "fish", "fit", "fitness",
    "fix", "flag", "flame", "flash", "flat", "flavor", "flee", "flight",
    "flip", "float", "flock", "floor", "flower", "fluid", "flush", "fly",
    "foam", "focus", "fog", "foil", "fold", "follow", "food", "foot",
    "force", "forest", "forget", "fork", "fortune", "forum", "forward", "fossil",
    "foster", "found", "fox", "fragile", "frame", "frequent", "fresh", "friend",
    "fringe", "frog", "front", "frost", "frown", "frozen", "fruit", "fuel",
    "fun", "funny", "furnace", "fury", "future", "gadget", "gain", "galaxy",
    "gallery", "game", "gap", "garage", "garbage", "garden", "garlic", "garment",
    "gas", "gasp", "gate", "gather", "gauge", "gaze", "general", "genius",
    "genre", "gentle", "genuine", "gesture", "ghost", "giant", "gift", "giggle",
    "ginger", "giraffe", "girl", "give", "glad", "glance", "glare", "glass",
    "glide", "glimpse", "globe", "gloom", "glory", "glove", "glow", "glue",
    "goat", "goddess", "gold", "good", "goose", "gorilla", "gospel", "gossip",
    "govern", "gown", "grab", "grace", "grain", "grant", "grape", "grass",
    "gravity", "great", "green", "grid", "grief", "grit", "grocery", "group",
    "grow", "grunt", "guard", "guess", "guide", "guilt", "guitar", "gun",
    "gym", "habit", "hair", "half", "hammer", "hamster", "hand", "happy",
    "harbor", "hard", "harsh", "harvest", "hat", "have", "hawk", "hazard",
    "head", "health", "heart", "heavy", "hedgehog", "height", "hello", "helmet",
    "help", "hen", "hero", "hidden", "high", "hill", "hint", "hip",
    "hire", "history", "hobby", "hockey", "hold", "hole", "holiday", "hollow",
    "home", "honey", "hood", "hope", "horn", "horror", "horse", "hospital",
    "host", "hotel", "hour", "hover", "hub", "huge", "human", "humble",
    "humor", "hundred", "hungry", "hunt", "hurdle", "hurry", "hurt", "husband",
    "hybrid", "ice", "icon", "idea", "identify", "idle", "ignore", "ill",
    "illegal", "illness", "image", "imitate", "immense", "immune", "impact", "impose",
    "improve", "impulse", "inch", "include", "income", "increase", "index", "indicate",
    "indoor", "industry", "infant", "inflict", "inform", "inhale", "inherit", "initial",
    "inject", "injury", "inmate", "inner", "innocent", "input", "inquiry", "insane",
    "insect", "inside", "inspire", "install", "intact", "interest", "into", "invest",
    "invite", "involve", "iron", "island", "isolate", "issue", "item", "ivory",
    "jacket", "jaguar", "jar", "jazz", "jealous", "jeans", "jelly", "jewel",
    "job", "join", "joke", "journey", "joy", "judge", "juice", "jump",
    "jungle", "junior", "junk", "just", "kangaroo", "keen", "keep", "ketchup",
    "key", "kick", "kid", "kidney", "kind", "kingdom", "kiss", "kit",
    "kitchen", "kite", "kitten", "kiwi", "knee", "knife", "knock", "know",
    "lab", "label", "labor", "ladder", "lady", "lake", "lamp", "language",
    "laptop", "large", "later", "latin", "laugh", "laundry", "lava", "law",
    "lawn", "lawsuit", "layer", "lazy", "leader", "leaf", "learn", "leave",
    "lecture", "left", "leg", "legal", "legend", "leisure", "lemon", "lend",
    "length", "lens", "leopard", "lesson", "letter", "level", "liar", "liberty",
    "library", "license", "life", "lift", "light", "like", "limb", "limit",
    "link", "lion", "liquid", "list", "little", "live", "lizard", "load",
    "loan", "lobster", "local", "lock", "logic", "lonely", "long", "loop",
    "lottery", "loud", "lounge", "love", "loyal", "lucky", "luggage", "lumber",
    "lunar", "lunch", "luxury", "lyrics", "machine", "mad", "magic", "magnet",
    "maid", "mail", "main", "major", "make", "mammal", "man", "manage",
    "mandate", "mango", "mansion", "manual", "maple", "marble", "march", "margin",
    "marine", "market", "marriage", "mask", "mass", "master", "match", "material",
    "math", "matrix", "matter", "maximum", "maze", "meadow", "mean", "measure",
    "meat", "mechanic", "medal", "media", "melody", "melt", "member", "memory",
    "mention", "menu", "mercy", "merge", "merit", "merry", "mesh", "message",
    "metal", "method", "middle", "midnight", "milk", "million", "mimic", "mind",
    "minimum", "minor", "minute", "miracle", "mirror", "misery", "miss", "mistake",
    "mix", "mixed", "mixture", "mobile", "model", "modify", "mom", "moment",
    "monitor", "monkey", "monster", "month", "moon", "moral", "more", "morning",
    "mosquito", "mother", "motion", "motor", "mountain", "mouse", "move", "movie",
    "much", "muffin", "mule", "multiply", "muscle", "museum", "mushroom", "music",
    "must", "mutual", "myself", "mystery", "myth", "naive", "name", "napkin",
    "narrow", "nasty", "nation", "nature", "near", "neck", "need", "negative",
    "neglect", "neither", "nephew", "nerve", "nest", "net", "network", "neutral",
    "never", "news", "next", "nice", "night", "noble", "noise", "nominee",
    "noodle", "normal", "north", "nose", "notable", "note", "nothing", "notice",
    "novel", "now", "nuclear", "number", "nurse", "nut", "oak", "obey",
    "object", "oblige", "obscure", "observe", "obtain", "obvious", "occur", "ocean",
    "october", "odor", "off", "offer", "office", "often", "oil", "okay",
    "old", "olive", "olympic", "omit", "once", "one", "onion", "online",
    "only", "open", "opera", "opinion", "oppose", "option", "orange", "orbit",
    "orchard", "order", "ordinary", "organ", "orient", "original", "orphan", "ostrich",
    "other", "outdoor", "outer", "output", "outside", "oval", "oven", "over",
    "own", "owner", "oxygen", "oyster", "ozone", "pact", "paddle", "page",
    "pair", "palace", "palm", "panda", "panel", "panic", "panther", "paper",
    "parade", "parent", "park", "parrot", "party", "pass", "patch", "path",
    "patient", "patrol", "pattern", "pause", "pave", "payment", "peace", "peanut",
    "pear", "peasant", "pelican", "pen", "penalty", "pencil", "people", "pepper",
    "perfect", "permit", "person", "pet", "phone", "photo", "phrase", "physical",
    "piano", "picnic", "picture", "piece", "pig", "pigeon", "pill", "pilot",
    "pink", "pioneer", "pipe", "pistol", "pitch", "pizza", "place", "planet",
    "plastic", "plate", "play", "please", "pledge", "pluck", "plug", "plunge",
    "poem", "poet", "point", "polar", "pole", "police", "pond", "pony",
    "pool", "popular", "portion", "position", "possible", "post", "potato", "pottery",
    "poverty", "powder", "power", "practice", "praise", "predict", "prefer", "prepare",
    "present", "pretty", "prevent", "price", "pride", "primary", "print", "priority",
    "prison", "private", "prize", "problem", "process", "produce", "profit", "program",
    "project", "promote", "proof", "property", "prosper", "protect", "proud", "provide",
    "public", "pudding", "pull", "pulp", "pulse", "pumpkin", "punch", "pupil",
    "puppy", "purchase", "purity", "purpose", "purse", "push", "put", "puzzle",
    "pyramid", "quality", "quantum", "quarter", "question", "quick", "quit", "quiz",
    "quote", "rabbit", "raccoon", "race", "rack", "radar", "radio", "rail",
    "rain", "raise", "rally", "ramp", "ranch", "random", "range", "rapid",
    "rare", "rate", "rather", "raven", "raw", "razor", "ready", "real",
    "reason", "rebel", "rebuild", "recall", "receive", "recipe", "record", "recycle",
    "reduce", "reflect", "reform", "refuse", "region", "regret", "regular", "reject",
    "relax", "release", "relief", "rely", "remain", "remember", "remind", "remove",
    "render", "renew", "rent", "reopen", "repair", "repeat", "replace", "report",
    "require", "rescue", "resemble", "resist", "resource", "response", "result", "retire",
    "retreat", "return", "reunion", "reveal", "review", "reward", "rhythm", "rib",
    "ribbon", "rice", "rich", "ride", "ridge", "rifle", "right", "rigid",
    "ring", "riot", "ripple", "risk", "ritual", "rival", "river", "road",
    "roast", "robot", "robust", "rocket", "romance", "roof", "rookie", "room",
    "rose", "rotate", "rough", "round", "route", "royal", "rubber", "rude",
    "rug", "rule", "run", "runway", "rural", "sad", "saddle", "sadness",
    "safe", "sail", "salad", "salmon", "salon", "salt", "salute", "same",
    "sample", "sand", "satisfy", "satoshi", "sauce", "sausage", "save", "say",
    "scale", "scan", "scare", "scatter", "scene", "scheme", "school", "science",
    "scissors", "scorpion", "scout", "scrap", "screen", "script", "scrub", "sea",
    "search", "season", "seat", "second", "secret", "section", "security", "seed",
    "seek", "segment", "select", "sell", "seminar", "senior", "sense", "sentence",
    "series", "service", "session", "settle", "setup", "seven", "shadow", "shaft",
    "shallow", "share", "shed", "shell", "sheriff", "shield", "shift", "shine",
    "ship", "shiver", "shock", "shoe", "shoot", "shop", "short", "shoulder",
    "shove", "shrimp", "shrug", "shuffle", "shy", "sibling", "sick", "side",
    "siege", "sight", "sign", "silent", "silk", "silly", "silver", "similar",
    "simple", "since", "sing", "siren", "sister", "situate", "six", "size",
    "skate", "sketch", "ski", "skill", "skin", "skirt", "skull", "slab",
    "slam", "sleep", "slender", "slice", "slide", "slight", "slim", "slogan",
    "slot", "slow", "slush", "small", "smart", "smile", "smoke", "smooth",
    "snack", "snake", "snap", "sniff", "snow", "soap", "soccer", "social",
    "sock", "soda", "soft", "solar", "soldier", "solid", "solution", "solve",
    "someone", "song", "soon", "sorry", "sort", "soul", "sound", "soup",
    "source", "south", "space", "spare", "spatial", "spawn", "speak", "special",
    "speed", "spell", "spend", "sphere", "spice", "spider", "spike", "spin",
    "spirit", "split", "spoil", "sponsor", "spoon", "sport", "spot", "spray",
    "spread", "spring", "spy", "square", "squeeze", "squirrel", "stable", "stadium",
    "staff", "stage", "stairs", "stamp", "stand", "start", "state", "stay",
    "steak", "steel", "stem", "step", "stereo", "stick", "still", "sting",
    "stock", "stomach", "stone", "stool", "story", "stove", "strategy", "street",
    "strike", "strong", "struggle", "student", "stuff", "stumble", "style", "subject",
    "submit", "subway", "success", "such", "sudden", "suffer", "sugar", "suggest",
    "suit", "summer", "sun", "sunny", "sunset", "super", "supply", "supreme",
    "sure", "surface", "surge", "surprise", "surround", "survey", "suspect", "sustain",
    "swallow", "swamp", "swap", "swarm", "swear", "sweet", "swift", "swim",
    "swing", "switch", "sword", "symbol", "symptom", "syrup", "system", "table",
    "tackle", "tag", "tail", "talent", "talk", "tank", "tape", "target",
    "task", "taste", "tattoo", "taxi", "teach", "team", "tell", "ten",
    "tenant", "tennis", "tent", "term", "test", "text", "thank", "that",
    "theme", "then", "theory", "there", "they", "thing", "this", "thought",
    "three", "thrive", "throw", "thumb", "thunder", "ticket", "tide", "tiger",
    "tilt", "timber", "time", "tiny", "tip", "tired", "tissue", "title",
    "toast", "tobacco", "today", "toddler", "toe", "together", "toilet", "token",
    "tomato", "tomorrow", "tone", "tongue", "tonight", "tool", "tooth", "top",
    "topic", "topple", "torch", "tornado", "tortoise", "toss", "total", "tourist",
    "toward", "tower", "town", "toy", "track", "trade", "traffic", "tragic",
    "train", "transfer", "trap", "trash", "travel", "tray", "treat", "tree",
    "trend", "trial", "tribe", "trick", "trigger", "trim", "trip", "trophy",
    "trouble", "truck", "true", "truly", "trumpet", "trust", "truth", "try",
    "tube", "tuition", "tumble", "tuna", "tunnel", "turkey", "turn", "turtle",
    "twelve", "twenty", "twice", "twin", "twist", "two", "type", "typical",
    "ugly", "umbrella", "unable", "unaware", "uncle", "uncover", "under", "undo",
    "unfair", "unfold", "unhappy", "uniform", "unique", "unit", "universe", "unknown",
    "unlock", "until", "unusual", "unveil", "update", "upgrade", "uphold", "upon",
    "upper", "upset", "urban", "urge", "usage", "use", "used", "useful",
    "useless", "usual", "utility", "vacant", "vacuum", "vague", "valid", "valley",
    "valve", "van", "vanish", "vapor", "various", "vast", "vault", "vehicle",
    "velvet", "vendor", "venture", "venue", "verb", "verify", "version", "very",
    "vessel", "veteran", "viable", "vibrant", "vicious", "victory", "video", "view",
    "village", "vintage", "violin", "virtual", "virus", "visa", "visit", "visual",
    "vital", "vivid", "vocal", "voice", "void", "volcano", "volume", "vote",
    "voyage", "wage", "wagon", "wait", "walk", "wall", "walnut", "want",
    "warfare", "warm", "warrior", "wash", "wasp", "waste", "water", "wave",
    "way", "wealth", "weapon", "wear", "weasel", "weather", "web", "wedding",
    "weekend", "weird", "welcome", "west", "wet", "whale", "what", "wheat",
    "wheel", "when", "where", "whip", "whisper", "wide", "width", "wife",
    "wild", "will", "win", "window", "wine", "wing", "wink", "winner",
    "winter", "wire", "wisdom", "wise", "wish", "witness", "wolf", "woman",
    "wonder", "wood", "wool", "word", "work", "world", "worry", "worth",
    "wrap", "wreck", "wrestle", "wrist", "write", "wrong", "yard", "year",
    "yellow", "you", "young", "youth", "zebra", "zero", "zone", "zoo",
]

if weechat is not None:
    DB_PATH = ""
else:
    DB_PATH = os.path.expanduser("~/.weechat/rpe2e.db")

_rate_limit_sent: dict[str, float] = {}

_incoming_buckets: dict[str, "IncomingBucket"] = {}

# Our own server-stamped ident@host, per server — the recipient-keyed DM
# context (docs/rpe2e-dm-addendum.md): DMs we RECEIVE are keyed `@<own>`.
# VOLATILE by design (never persisted): reset at (dis)connect and re-seeded,
# because the server may assign a different ident/host/cloak each session.
#
# Sources are RANKED, because they disagree on cloaked networks: what matters
# is the handle as PEERS see it (our message prefix), and solanum-family ircds
# (Libera) answer a self-USERHOST with the REAL host, not the cloak.
#   rank 2 — prefix-visible: echo-message echoes, our own JOIN, our own
#            CHGHOST, RPL_HOSTHIDDEN (396). Authoritative.
#   rank 1 — self-USERHOST (302) reply: seed of last resort (a DM-only user
#            on zero channels produces no rank-2 event until they speak).
# A lower rank never overwrites a higher one; reads prefer rank 2, then a live
# own-nicklist lookup (kept current by weechat incl. CHGHOST), then rank 1.
# Values: {"handle": str, "rank": int}.
_own_handle: dict[str, dict] = {}

# Throttle for the "held — own identity not learned yet" notice, per
# (server, sender nick), so a chatty peer doesn't flood the buffer while we
# wait for the USERHOST reply.
_own_wait_notice_at: dict[tuple, float] = {}


class IncomingBucket:
    __slots__ = ("recent", "backoff_until")

    def __init__(self):
        self.recent: list[float] = []
        self.backoff_until: float = 0.0


def _allow_incoming(handle: str) -> bool:
    now = time.time()
    bucket = _incoming_buckets.get(handle)
    if bucket is None:
        bucket = IncomingBucket()
        _incoming_buckets[handle] = bucket
    if bucket.backoff_until > 0 and now < bucket.backoff_until:
        return False
    if bucket.backoff_until > 0 and now >= bucket.backoff_until:
        bucket.backoff_until = 0.0
        bucket.recent.clear()
    bucket.recent = [t for t in bucket.recent if now - t < INCOMING_WINDOW_SECS]
    if len(bucket.recent) >= INCOMING_MAX_PER_WINDOW:
        bucket.backoff_until = now + INCOMING_BACKOFF_SECS
        return False
    bucket.recent.append(now)
    return True


@contextmanager
def db_conn():
    if not DB_PATH:
        raise RuntimeError("rpe2e: DB_PATH not initialized")
    os.makedirs(os.path.dirname(DB_PATH), exist_ok=True)
    conn = sqlite3.connect(DB_PATH)
    try:
        yield conn
        # Persist on success. sqlite3 opens an implicit transaction for every
        # INSERT/UPDATE/DELETE in legacy isolation mode; without this commit the
        # transaction is rolled back on close() and NOTHING is written — which
        # would leave `/e2e on`, session keys, and peer trust un-persisted and
        # silently downgrade every "enabled" conversation to plaintext (the gate
        # would always read "not enabled"). Commit-on-success / rollback-on-error.
        conn.commit()
    except Exception:
        conn.rollback()
        raise
    finally:
        conn.close()


SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS identity (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    pk          BLOB NOT NULL,
    sk          BLOB NOT NULL,
    fp          BLOB NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS peers (
    fp           BLOB PRIMARY KEY,
    pk           BLOB NOT NULL,
    last_handle  TEXT,
    last_nick    TEXT,
    first_seen   INTEGER,
    last_seen    INTEGER,
    status       TEXT DEFAULT 'pending'
);
CREATE TABLE IF NOT EXISTS outgoing (
    channel           TEXT PRIMARY KEY,
    sk                BLOB NOT NULL,
    created_at        INTEGER NOT NULL,
    pending_rotation  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS incoming (
    handle      TEXT NOT NULL,
    channel     TEXT NOT NULL,
    fp          BLOB NOT NULL,
    sk          BLOB NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending',
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (handle, channel)
);
CREATE TABLE IF NOT EXISTS channels (
    channel TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL DEFAULT 0,
    mode    TEXT NOT NULL DEFAULT 'normal'
);
CREATE TABLE IF NOT EXISTS pending (
    channel     TEXT PRIMARY KEY,
    eph_sk      BLOB NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS autotrust (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    scope          TEXT NOT NULL,
    handle_pattern TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    UNIQUE(scope, handle_pattern)
);
CREATE TABLE IF NOT EXISTS outgoing_recipients (
    channel        TEXT NOT NULL,
    handle         TEXT NOT NULL,
    fingerprint   BLOB NOT NULL,
    first_sent_at INTEGER NOT NULL,
    PRIMARY KEY (channel, handle)
);
CREATE TABLE IF NOT EXISTS pending_inbound (
    handle        TEXT NOT NULL,
    channel       TEXT NOT NULL,
    sender_handle TEXT NOT NULL,
    pubkey        BLOB NOT NULL,
    eph_x25519    BLOB NOT NULL,
    nonce         BLOB NOT NULL,
    sig           BLOB NOT NULL,
    received_at   INTEGER NOT NULL,
    PRIMARY KEY (handle, channel)
);
CREATE TABLE IF NOT EXISTS pending_trust_change (
    handle      TEXT NOT NULL,
    channel     TEXT NOT NULL,
    change      TEXT NOT NULL,
    new_pubkey  BLOB,
    old_fp      BLOB,
    new_fp      BLOB,
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY (handle, channel)
);
"""


def init_db() -> None:
    with db_conn() as c:
        c.execute("PRAGMA journal_mode=WAL")
        c.executescript(SCHEMA_SQL)
        c.execute("DELETE FROM pending")


def _pending_key(channel: str, handle: str | None = None) -> str:
    return f"{channel}\x1f{handle}" if handle else channel


def context_key(target: str, handle: str) -> str:
    if target and target[0] in CHANNEL_PREFIXES:
        return target
    return "@" + handle


def _parse_userhost_reply(entry: str):
    """One RPL_USERHOST (302) entry `nick[*]=[+|-]ident@host` → (nick, handle)
    or None. Mirrors Rust `parse_userhost_reply`: the trailing `*` on the nick
    marks an oper (stripped), the leading `+`/`-` on the userhost is the away
    flag (stripped); the handle keeps `~` and any cloak verbatim."""
    if "=" not in entry:
        return None
    nick_part, userhost = entry.split("=", 1)
    nick_part = nick_part.rstrip("*")
    if userhost[:1] in ("+", "-"):
        userhost = userhost[1:]
    if not nick_part or "@" not in userhost:
        return None
    return nick_part, userhost


def _own_nick(server: str) -> str:
    return weechat.info_get("irc_nick", server) if weechat else ""


def _set_own_handle(server: str, handle: str, rank: int = 2) -> None:
    if not handle:
        return
    cur = _own_handle.get(server)
    if cur and cur["rank"] > rank:
        return  # a prefix-visible value never yields to a self-USERHOST one
    if not cur or cur["handle"] != handle or cur["rank"] != rank:
        _own_handle[server] = {"handle": handle, "rank": rank}
        _dbg(f"own handle on {server}: {handle} (rank {rank})")


def _own_handle_get(server: str):
    """Best current view of our own peer-visible handle, or None.
    Priority: rank-2 store → our own nick on any joined channel's nicklist
    (weechat keeps it current, incl. CHGHOST) → rank-1 store (USERHOST)."""
    entry = _own_handle.get(server)
    if entry and entry["rank"] >= 2:
        return entry["handle"]
    if weechat is not None:
        nick = _own_nick(server)
        if nick:
            infolist = weechat.infolist_get("irc_channel", "", server)
            if infolist:
                found = None
                try:
                    while weechat.infolist_next(infolist):
                        chan = weechat.infolist_string(infolist, "name") or ""
                        if not chan:
                            continue
                        found = _resolve_handle_by_nick(server, chan, nick)
                        if found:
                            break
                finally:
                    weechat.infolist_free(infolist)
                if found:
                    return found
    return entry["handle"] if entry else None


def _send_self_userhost(server: str) -> None:
    """ONE-SHOT self-USERHOST to seed our own handle — gated to connect/load
    events, never per message (the 302 reply is itself a message; a per-message
    trigger would loop). Mirrors the Rust client's RPL_WELCOME one-shot."""
    if weechat is None:
        return
    nick = _own_nick(server)
    buf = weechat.buffer_search("irc", f"server.{server}") or ""
    if nick and buf:
        weechat.command(buf, f"/quote USERHOST {nick}")


def _incoming_ctx_for(server: str, ctx: str):
    """Context that REAL incoming DM sessions live under: `@<own_handle>`
    (recipient-keyed — the recipient of the direction is us). Channels pass
    through unchanged. None → our own handle is not known yet."""
    if ctx.startswith("@"):
        own_h = _own_handle_get(server)
        return ("@" + own_h) if own_h else None
    return ctx


def _update_incoming_trust(handle: str, ctx: str, status: str) -> int:
    """Set the trust status of a peer's incoming session(s); returns the row
    count. A DM trust change targets the PEER, not one context string: the
    real session lives under `@<own>` (which may be UNKNOWN right now, e.g.
    right after reconnect/reload before USERHOST/JOIN/396 seeds it), the
    trust marker under `@<peer>`, plus possibly stale rows from handle drift
    — so update EVERY DM row for the handle. Touching only the resolvable
    context would report success while leaving the trusted `@<own>` session
    decryptable once the handle is learned. Channels update exactly
    (handle, ctx)."""
    with db_conn() as c:
        if ctx.startswith("@"):
            cur = c.execute(
                "UPDATE incoming SET status=? WHERE handle = ? AND channel LIKE '@%'",
                (status, handle),
            )
        else:
            cur = c.execute(
                "UPDATE incoming SET status=? WHERE handle = ? AND channel = ?",
                (status, handle, ctx),
            )
        return cur.rowcount


def _delete_incoming_rows(handle: str, ctx: str) -> int:
    """Forget a peer's incoming session(s); same handle-wide DM semantics as
    `_update_incoming_trust` (see there for why)."""
    with db_conn() as c:
        if ctx.startswith("@"):
            cur = c.execute(
                "DELETE FROM incoming WHERE handle = ? AND channel LIKE '@%'",
                (handle,),
            )
        else:
            cur = c.execute(
                "DELETE FROM incoming WHERE handle = ? AND channel = ?",
                (handle, ctx),
            )
        return cur.rowcount


def fingerprint(pk: bytes) -> bytes:
    return hashlib.sha256(b"RPE2E01-FP:" + pk).digest()[:16]


def fingerprint_hex(fp: bytes) -> str:
    return fp.hex()


def fingerprint_bip39(fp_bytes: bytes) -> str:
    assert len(fp_bytes) == 16
    h = hashlib.sha256(fp_bytes).digest()
    checksum = (h[0] >> 4) & 0xF
    entropy_int = int.from_bytes(fp_bytes, "big")
    combined = (entropy_int << 4) | checksum
    words = []
    for i in range(12):
        shift = 132 - 11 * (i + 1)
        idx = (combined >> shift) & 0x7FF
        words.append(BIP39_WORDS[idx])
    return " ".join(words[:6])


def hkdf_sha256(salt: bytes, ikm: bytes, info: bytes, length: int) -> bytes:
    prk = hmac_mod.new(salt, ikm, hashlib.sha256).digest()
    out = b""
    prev = b""
    counter = 1
    while len(out) < length:
        prev = hmac_mod.new(
            prk, prev + info + bytes([counter]), hashlib.sha256
        ).digest()
        out += prev
        counter += 1
    return out[:length]


def build_aad(channel: str, msgid: bytes, ts: int, part: int, total: int) -> bytes:
    """Byte-identical to Rust `src/e2e/wire.rs::build_aad`.

    AAD layout (length-prefixed, big-endian):
        PROTO(7 bytes, fixed)
          || be16(channel.len) || channel
          || be16(8)  || msgid (8 bytes)
          || be16(8)  || ts_be (8 bytes)
          || be16(1)  || part  (1 byte)
          || be16(1)  || total (1 byte)
    """
    chan_bytes = channel.encode()
    return (
        PROTO.encode()
        + struct.pack(">H", len(chan_bytes))
        + chan_bytes
        + struct.pack(">H", 8)
        + msgid
        + struct.pack(">H", 8)
        + struct.pack(">q", ts)
        + struct.pack(">H", 1)
        + bytes([part])
        + struct.pack(">H", 1)
        + bytes([total])
    )


def aead_encrypt(key: bytes, aad: bytes, pt: bytes) -> tuple[bytes, bytes]:
    nonce = nacl_random(NONCE_LEN)
    ct = crypto_aead_xchacha20poly1305_ietf_encrypt(pt, aad, nonce, key)
    return nonce, ct


def aead_decrypt(key: bytes, nonce: bytes, aad: bytes, ct: bytes) -> bytes | None:
    try:
        return crypto_aead_xchacha20poly1305_ietf_decrypt(ct, aad, nonce, key)
    except Exception:
        return None


def ensure_identity() -> tuple[bytes, bytes, bytes]:
    with db_conn() as c:
        row = c.execute("SELECT pk, sk, fp FROM identity WHERE id = 1").fetchone()
        if row is not None:
            return row[0], row[1], row[2]
        sk_obj = SigningKey.generate()
        pk = bytes(sk_obj.verify_key)
        sk = bytes(sk_obj)
        fp = fingerprint(pk)
        c.execute(
            "INSERT INTO identity VALUES (1, ?, ?, ?, ?)",
            (pk, sk, fp, int(time.time())),
        )
        return pk, sk, fp


def ed25519_sign(sk_bytes: bytes, msg: bytes) -> bytes:
    signing = SigningKey(sk_bytes)
    return signing.sign(msg).signature


def ed25519_verify(pk_bytes: bytes, msg: bytes, sig: bytes) -> bool:
    try:
        VerifyKey(pk_bytes).verify(msg, sig)
        return True
    except (BadSignatureError, Exception):
        return False


def generate_x25519_keypair() -> tuple[bytes, bytes]:
    sk_arr = bytearray(nacl_random(32))
    sk_arr[0] &= 248
    sk_arr[31] &= 127
    sk_arr[31] |= 64
    sk = bytes(sk_arr)
    pk = crypto_scalarmult_base(sk)
    return sk, pk


def x25519_ecdh(sk: bytes, peer_pk: bytes) -> bytes:
    return crypto_scalarmult(sk, peer_pk)


def ed25519_pk_to_x25519(ed_pk: bytes) -> bytes:
    return crypto_sign_ed25519_pk_to_curve25519(ed_pk)


def ed25519_sk_to_x25519_scalar(ed_sk: bytes, ed_pk: bytes) -> bytes:
    expanded = ed_sk + ed_pk
    return crypto_sign_ed25519_sk_to_curve25519(expanded)


def _sig_payload_keyreq(
    channel: str, pub: bytes, eph_x25519: bytes, nonce: bytes
) -> bytes:
    return b"KEYREQ:" + channel.encode() + b":" + pub + b":" + eph_x25519 + b":" + nonce


def _sig_payload_keyrsp(
    channel: str,
    pub: bytes,
    eph_pub: bytes,
    wnonce: bytes,
    wrap_ct: bytes,
    nonce: bytes,
) -> bytes:
    return (
        b"KEYRSP:"
        + channel.encode()
        + b":"
        + pub
        + b":"
        + eph_pub
        + b":"
        + wnonce
        + b":"
        + wrap_ct
        + b":"
        + nonce
    )


def _sig_payload_keyrekey(
    channel: str,
    pub: bytes,
    eph_pub: bytes,
    wnonce: bytes,
    wrap_ct: bytes,
    nonce: bytes,
) -> bytes:
    return (
        b"REKEY:"
        + channel.encode()
        + b":"
        + pub
        + b":"
        + eph_pub
        + b":"
        + wnonce
        + b":"
        + wrap_ct
        + b":"
        + nonce
    )


def _classify_peer_change(fp: bytes, handle: str) -> str:
    with db_conn() as c:
        row = c.execute(
            "SELECT pk, last_handle, status FROM peers WHERE fp = ?", (fp,)
        ).fetchone()
        if row is not None:
            _, last_handle, status = row
            if status == "revoked":
                return "revoked"
            if last_handle != handle:
                return "handle_changed:" + (last_handle or "")
            return "known"
        by_handle = c.execute(
            "SELECT fp FROM peers WHERE last_handle = ?", (handle,)
        ).fetchone()
        if by_handle is not None:
            return "fingerprint_changed:" + by_handle[0].hex()
        return "new"


def _glob_matches_ci(pattern: str, text: str) -> bool:
    return fnmatch.fnmatch(text.lower(), pattern.lower())


def _autotrust_matches(handle: str, channel: str) -> bool:
    with db_conn() as c:
        rows = c.execute(
            "SELECT handle_pattern FROM autotrust WHERE scope = 'global' OR scope = ?",
            (channel,),
        ).fetchall()
    for (pat,) in rows:
        if _glob_matches_ci(pat, handle):
            return True
    return False


def _record_pending_trust_change(
    handle: str,
    channel: str,
    change: str,
    new_pubkey: bytes | None = None,
    old_fp: bytes | None = None,
    new_fp: bytes | None = None,
) -> None:
    with db_conn() as c:
        c.execute(
            "INSERT OR REPLACE INTO pending_trust_change "
            "(handle, channel, change, new_pubkey, old_fp, new_fp, recorded_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?)",
            (handle, channel, change, new_pubkey, old_fp, new_fp, int(time.time())),
        )


def _take_pending_trust_changes(handle: str) -> list:
    """Drain pending_trust_change rows for `handle`. Returns a list of
    (channel, change, new_pubkey, old_fp, new_fp) tuples."""
    with db_conn() as c:
        rows = c.execute(
            "SELECT channel, change, new_pubkey, old_fp, new_fp "
            "FROM pending_trust_change WHERE handle = ?",
            (handle,),
        ).fetchall()
        c.execute("DELETE FROM pending_trust_change WHERE handle = ?", (handle,))
    return rows


def _resolve_handle_by_nick(server: str, channel: str, nick: str) -> str | None:
    """Resolve `nick` → `ident@host` by walking the weechat nicklist for
    `server.channel`. Returns None if the nick is not found. Falls back
    to the caller which then treats the nick itself as the handle."""
    if weechat is None:
        return None
    if not channel or channel[0] not in CHANNEL_PREFIXES:
        # PM / query buffer — look up the nick in the server's nicks infolist
        infolist = weechat.infolist_get("irc_nick", "", f"{server},{channel},{nick}")
        if infolist:
            try:
                if weechat.infolist_next(infolist):
                    host = weechat.infolist_string(infolist, "host") or ""
                    if host:
                        return host
            finally:
                weechat.infolist_free(infolist)
        # Fallback: query-buffer local var may carry the remote host
        buf = weechat.buffer_search("irc", f"{server}.{nick}") or ""
        if buf:
            host = weechat.buffer_get_string(buf, "localvar_host") or ""
            if host:
                return host
        return None
    buf = weechat.buffer_search("irc", f"{server}.{channel}")
    if not buf:
        return None
    nick_ptr = weechat.nicklist_search_nick(buf, "", nick)
    if not nick_ptr:
        # try lower-case fallback
        infolist = weechat.infolist_get("irc_nick", "", f"{server},{channel},*")
        if infolist:
            try:
                while weechat.infolist_next(infolist):
                    n = weechat.infolist_string(infolist, "name") or ""
                    if n.lower() == nick.lower():
                        host = weechat.infolist_string(infolist, "host") or ""
                        if host:
                            return host
            finally:
                weechat.infolist_free(infolist)
        return None
    # Walk the irc_nick infolist to find the matching host
    infolist = weechat.infolist_get("irc_nick", "", f"{server},{channel},{nick}")
    if not infolist:
        return None
    try:
        if weechat.infolist_next(infolist):
            host = weechat.infolist_string(infolist, "host") or ""
            if host:
                return host
    finally:
        weechat.infolist_free(infolist)
    return None


def _resolve_cached_handle_by_nick(nick: str) -> str | None:
    with db_conn() as c:
        rows = c.execute(
            "SELECT last_handle, last_nick, last_seen FROM peers WHERE last_nick IS NOT NULL"
        ).fetchall()
    nick_lower = nick.lower()
    matches = [
        (last_seen, last_handle)
        for last_handle, last_nick, last_seen in rows
        if last_handle
        and isinstance(last_handle, str)
        and isinstance(last_nick, str)
        and last_nick.lower() == nick_lower
    ]
    if not matches:
        return None
    matches.sort(key=lambda item: item[0])
    return matches[-1][1]


def _ctx_for_command(buffer_ptr, server: str, target: str, nick: str | None) -> str | None:
    """Figure out the E2E `channel` key for a /e2e subcommand running in
    `buffer_ptr`. For channel buffers the channel name is returned
    verbatim. For query buffers we resolve the handle by asking weechat
    for the nick's `ident@host`; if that fails (query against an
    offline/unknown nick) the command path should error out, so we
    return None. `nick` is the subcommand's <nick> argument, used to
    resolve the handle when the buffer itself is a channel.
    """
    if target and target[0] in CHANNEL_PREFIXES:
        return target
    # PM / query buffer: target is the remote nick (or the subcommand nick)
    peer_nick = nick or target
    if not peer_nick:
        return None
    handle = _resolve_handle_by_nick(server, target, peer_nick)
    if handle is None:
        return None
    return "@" + handle


def _resolve_handle_for_command(server: str, target: str, nick: str) -> str | None:
    if "@" in nick:
        return nick
    handle = _resolve_handle_by_nick(server, target, nick)
    if handle is not None:
        return handle
    return _resolve_cached_handle_by_nick(nick)


def _ctx_or_error(buf: str, buffer_ptr, server: str, target: str, nick: str | None, cmd: str) -> str | None:
    ctx = _ctx_for_command(buffer_ptr, server, target, nick)
    if ctx is None:
        who = nick or target or "peer"
        _prnt_err(buf, f"{cmd}: cannot resolve handle for {who} — has the user spoken yet?")
    return ctx


def _handle_or_error(buf: str, server: str, target: str, nick: str, cmd: str) -> str | None:
    handle = _resolve_handle_for_command(server, target, nick)
    if handle is None:
        _prnt_err(buf, f"{cmd}: cannot resolve handle for {nick} — has the user spoken yet?")
    return handle


def _prnt_ok(buf: str, msg: str) -> None:
    if weechat:
        weechat.prnt(buf, f"{C_OK}[E2E] {msg}{C_RST}")


def _prnt_err(buf: str, msg: str) -> None:
    if weechat:
        weechat.prnt(buf, f"{C_ERR}[E2E] {msg}{C_RST}")


def _prnt_warn(buf: str, msg: str) -> None:
    if weechat:
        weechat.prnt(buf, f"{C_WARN}[E2E] {msg}{C_RST}")


def _prnt_dbg(server: str, ctx: str, nick: str, msg: str) -> None:
    if not DEBUG_BUFFER_ENABLED or weechat is None:
        return
    buf = _notice_buffer_for_ctx(server, ctx, nick)
    weechat.prnt(buf, f"{C_INFO}[E2E debug] {msg}{C_RST}")


def _notice_buffer_for_ctx(server: str, ctx: str, nick: str) -> str:
    if weechat is None:
        return ""
    if ctx and ctx[0] in CHANNEL_PREFIXES:
        buf = weechat.buffer_search("irc", f"{server}.{ctx}") or ""
        if buf:
            return buf
    if nick:
        buf = weechat.buffer_search("irc", f"{server}.{nick}") or ""
        if buf:
            return buf
    # `weechat.buffer_search` does NOT accept wildcards — it does an
    # exact-name match. The canonical server buffer is
    # `server.<server>`, so try that as the last resort and fall back
    # to empty (callers that end up here should use
    # `_send_raw_notice` which passes `/quote -server` explicitly).
    return weechat.buffer_search("irc", f"server.{server}") or ""


def _send_raw_notice(server: str, nick: str, ctcp_body: str) -> bool:
    """Send an IRC NOTICE via the named server, independent of which
    weechat buffer happens to be active.

    Uses `/quote -server <server>` so the command is bound to the
    correct IRC connection even when dispatched from the core buffer
    or another server's buffer. Returns True on success, False if we
    could not resolve any IRC buffer to dispatch the command on.

    This exists because `weechat.buffer_search("irc", f"{server}.*")`
    does NOT accept wildcards — it does a literal match on the name
    string, so it always returns an empty buffer pointer. Commands
    dispatched on an empty buffer land in core.weechat and `/quote`
    with no `-server` flag has no IRC context → the NOTICE never
    leaves weechat. That was the exact failure mode for the
    reciprocal KEYRSP from `hook_irc_in_notice`."""
    if weechat is None:
        return False
    # Any IRC-plugin buffer works as a dispatch point because the
    # `/quote -server <name>` flag routes the command to the correct
    # server regardless of the buffer's own context. Prefer the
    # server buffer, fall back to any IRC buffer on that server, and
    # finally to the core buffer.
    buf = weechat.buffer_search("irc", f"server.{server}") or ""
    if not buf:
        # Scan for any irc buffer belonging to this server. We walk
        # the gui_buffers infolist rather than guessing names.
        infolist = weechat.infolist_get("buffer", "", "")
        try:
            while weechat.infolist_next(infolist):
                plugin = weechat.infolist_string(infolist, "plugin_name")
                name = weechat.infolist_string(infolist, "name")
                if plugin == "irc" and name.startswith(f"{server}."):
                    buf = weechat.infolist_pointer(infolist, "pointer")
                    break
        finally:
            weechat.infolist_free(infolist)
    rc = weechat.command(buf, f"/quote -server {server} NOTICE {nick} :{ctcp_body}")
    _dbg(
        f"_send_raw_notice server={server} nick={nick} buf={buf!r} "
        f"rc={rc} body_len={len(ctcp_body)}"
    )
    return True


def _send_raw_privmsg(server: str, target: str, body: str) -> bool:
    if weechat is None:
        return False
    buf = weechat.buffer_search("irc", f"server.{server}") or ""
    if not buf:
        infolist = weechat.infolist_get("buffer", "", "")
        try:
            while weechat.infolist_next(infolist):
                plugin = weechat.infolist_string(infolist, "plugin_name")
                name = weechat.infolist_string(infolist, "name")
                if plugin == "irc" and name.startswith(f"{server}."):
                    buf = weechat.infolist_pointer(infolist, "pointer")
                    break
        finally:
            weechat.infolist_free(infolist)
    rc = weechat.command(buf, f"/quote -server {server} PRIVMSG {target} :{body}")
    _dbg(
        f"_send_raw_privmsg server={server} target={target} buf={buf!r} "
        f"rc={rc} body_len={len(body)}"
    )
    return True


def _prnt_self_msg(buf: str, text: str) -> None:
    if weechat is None or not buf:
        return
    nick = weechat.buffer_get_string(buf, "localvar_nick") or ""
    if not nick:
        server = weechat.buffer_get_string(buf, "localvar_server") or ""
        nick = weechat.info_get("irc_nick", server) or ""
    prefix = f"{weechat.color('chat_nick_self')}{nick}{C_RST}" if nick else ""
    tags = f"self_msg,notify_none,no_highlight,nick_{nick}" if nick else "self_msg,notify_none,no_highlight"
    weechat.prnt_date_tags(buf, 0, tags, f"{prefix}\t{text}")


def _allow_outgoing_keyreq(handle: str) -> bool:
    now_f = time.time()
    last = _rate_limit_sent.get(handle, 0.0)
    if now_f - last < KEYREQ_MIN_INTERVAL:
        return False
    _rate_limit_sent[handle] = now_f
    return True


def _parse_kv_strict(fields: list[str]) -> dict[str, str] | None:
    """Parse `k=v` fields with strict duplicate rejection.

    Mirrors Rust `src/e2e/handshake.rs::parse_kv` — if the same key appears
    twice in the same handshake body we return None rather than silently
    last-wins. An ambiguous body like `chan=#a chan=#b` could otherwise
    let a crafted payload shift the semantic channel of a signed
    KEYREQ/KEYRSP/REKEY after the fact.
    """
    out: dict[str, str] = {}
    for p in fields:
        if "=" in p:
            k, v = p.split("=", 1)
            if k in out:
                return None
            out[k] = v
    return out


def _b64u_encode(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).decode().rstrip("=")


def _b64u_decode(data: str) -> bytes:
    pad = "=" * (-len(data) % 4)
    return base64.urlsafe_b64decode(data + pad)


def parse_keyreq(body: str) -> dict | None:
    parts = body.split()
    if len(parts) < 7 or parts[0] != CTCP_TAG or parts[1] != "KEYREQ":
        return None
    kv = _parse_kv_strict(parts[2:])
    if kv is None:
        return None
    try:
        if kv.get("v") != "1":
            return None
        channel = kv["c"]
        pub = _b64u_decode(kv["p"])
        eph_x25519 = _b64u_decode(kv["e"])
        nonce = _b64u_decode(kv["n"])
        sig = _b64u_decode(kv["s"])
    except (KeyError, ValueError):
        return None
    if len(pub) != 32 or len(eph_x25519) != 32 or len(nonce) != 16 or len(sig) != 64:
        return None
    return {
        "channel": channel,
        "pub": pub,
        "eph_x25519": eph_x25519,
        "nonce": nonce,
        "sig": sig,
    }


def parse_keyrsp(body: str) -> dict | None:
    parts = body.split()
    if len(parts) < 9 or parts[0] != CTCP_TAG or parts[1] != "KEYRSP":
        return None
    kv = _parse_kv_strict(parts[2:])
    if kv is None:
        return None
    try:
        if kv.get("v") != "1":
            return None
        channel = kv["c"]
        pub = _b64u_decode(kv["p"])
        eph_pub = _b64u_decode(kv["e"])
        wnonce = _b64u_decode(kv["wn"])
        wrap_ct = _b64u_decode(kv["w"])
        nonce = _b64u_decode(kv["n"])
        sig = _b64u_decode(kv["s"])
    except (KeyError, ValueError):
        return None
    if (
        len(pub) != 32
        or len(eph_pub) != 32
        or len(wnonce) != NONCE_LEN
        or len(nonce) != 16
        or len(sig) != 64
    ):
        return None
    return {
        "channel": channel,
        "pub": pub,
        "eph_pub": eph_pub,
        "wrap_nonce": wnonce,
        "wrap_ct": wrap_ct,
        "nonce": nonce,
        "sig": sig,
    }


def parse_keyrekey(body: str) -> dict | None:
    parts = body.split()
    if len(parts) < 9 or parts[0] != CTCP_TAG or parts[1] != "REKEY":
        return None
    kv = _parse_kv_strict(parts[2:])
    if kv is None:
        return None
    try:
        if kv.get("v") != "1":
            return None
        channel = kv["c"]
        pub = _b64u_decode(kv["p"])
        eph_pub = _b64u_decode(kv["e"])
        wnonce = _b64u_decode(kv["wn"])
        wrap_ct = _b64u_decode(kv["w"])
        nonce = _b64u_decode(kv["n"])
        sig = _b64u_decode(kv["s"])
    except (KeyError, ValueError):
        return None
    if (
        len(pub) != 32
        or len(eph_pub) != 32
        or len(wnonce) != NONCE_LEN
        or len(nonce) != 16
        or len(sig) != 64
    ):
        return None
    return {
        "channel": channel,
        "pub": pub,
        "eph_pub": eph_pub,
        "wrap_nonce": wnonce,
        "wrap_ct": wrap_ct,
        "nonce": nonce,
        "sig": sig,
    }


def build_keyreq(channel: str, handle: str | None = None) -> str:
    pk, sk, _fp = ensure_identity()
    pending_key = _pending_key(channel, handle)
    with db_conn() as c:
        row = c.execute(
            "SELECT created_at FROM pending WHERE channel = ?", (pending_key,)
        ).fetchone()
    if row is not None:
        created_at = int(row[0])
        if int(time.time()) - created_at < PENDING_KEYREQ_TTL:
            raise ValueError(f"key exchange already pending for {pending_key}")
        with db_conn() as c:
            c.execute("DELETE FROM pending WHERE channel = ?", (pending_key,))
    eph_sk, eph_pk = generate_x25519_keypair()
    req_nonce = nacl_random(16)
    sig_payload = _sig_payload_keyreq(channel, pk, eph_pk, req_nonce)
    sig = ed25519_sign(sk, sig_payload)
    with db_conn() as c:
        c.execute(
            "INSERT OR REPLACE INTO pending VALUES (?, ?, ?)",
            (pending_key, eph_sk, int(time.time())),
        )
    body = (
        f"{CTCP_TAG} KEYREQ v=1 c={channel} p={_b64u_encode(pk)} "
        f"e={_b64u_encode(eph_pk)} n={_b64u_encode(req_nonce)} s={_b64u_encode(sig)}"
    )
    return "\x01" + body + "\x01"


def _build_keyrsp_for_req(
    channel: str, sender_handle: str, req_pub: bytes, req_eph: bytes
) -> str | None:
    pk, sk, _fp = ensure_identity()
    eph_sk, eph_pk = generate_x25519_keypair()
    shared = x25519_ecdh(eph_sk, req_eph)
    info = b"RPE2E01-WRAP:" + channel.encode()
    wrap_key = hkdf_sha256(HKDF_SALT, shared, info, KEY_LEN)
    our_sk_bytes = _get_or_generate_outgoing_key(channel)
    wrap_nonce, wrap_ct = aead_encrypt(wrap_key, info, our_sk_bytes)
    rsp_nonce = nacl_random(16)
    sig_payload = _sig_payload_keyrsp(
        channel, pk, eph_pk, wrap_nonce, wrap_ct, rsp_nonce
    )
    sig = ed25519_sign(sk, sig_payload)
    peer_fp = fingerprint(req_pub)
    now = int(time.time())
    with db_conn() as c:
        existing = c.execute(
            "SELECT first_seen FROM peers WHERE fp = ?", (peer_fp,)
        ).fetchone()
        first = existing[0] if existing else now
        c.execute(
            "INSERT OR REPLACE INTO peers VALUES (?, ?, ?, ?, ?, ?, 'trusted')",
            (peer_fp, req_pub, sender_handle, None, first, now),
        )
        c.execute(
            "INSERT OR REPLACE INTO outgoing_recipients (channel, handle, fingerprint, first_sent_at) VALUES (?, ?, ?, ?)",
            (channel, sender_handle, peer_fp, now),
        )
    body = (
        f"{CTCP_TAG} KEYRSP v=1 c={channel} p={_b64u_encode(pk)} "
        f"e={_b64u_encode(eph_pk)} wn={_b64u_encode(wrap_nonce)} "
        f"w={_b64u_encode(wrap_ct)} "
        f"n={_b64u_encode(rsp_nonce)} s={_b64u_encode(sig)}"
    )
    return "\x01" + body + "\x01"


def _reciprocal_ctx(channel: str, server: str) -> str | None:
    """Context for OUR reciprocal KEYREQ. A reciprocal establishes the
    peer→us direction, whose recipient-keyed context is OUR handle — not the
    requester's context from their KEYREQ (that names the other direction).
    Channels pass through. None → own handle unknown; skip the reciprocal
    (the transport self-heals later via auto-KEYREQ on their first wire)."""
    if channel[:1] in CHANNEL_PREFIXES:
        return channel
    own_h = _own_handle_get(server)
    return ("@" + own_h) if own_h else None


def _maybe_build_reciprocal_keyreq(channel: str, sender_handle: str, server: str) -> str | None:
    channel = _reciprocal_ctx(channel, server)
    if channel is None:
        _dbg("_maybe_build_reciprocal_keyreq: own handle unknown — skipping")
        return None
    with db_conn() as c:
        row = c.execute(
            "SELECT status FROM incoming WHERE handle = ? AND channel = ?",
            (sender_handle, channel),
        ).fetchone()
        pending = c.execute(
            "SELECT 1 FROM pending WHERE channel = ?",
            (_pending_key(channel, sender_handle),),
        ).fetchone()
    already_trusted = row is not None and row[0] == "trusted"
    allow = _allow_outgoing_keyreq(sender_handle)
    _dbg(
        f"_maybe_build_reciprocal_keyreq channel={channel} sender={sender_handle} "
        f"already_trusted={already_trusted} pending={pending} allow_outgoing={allow}"
    )
    if pending is not None or already_trusted or not allow:
        return None
    return build_keyreq(channel, sender_handle)


def _build_reciprocal_keyreq_on_accept(channel: str, sender_handle: str, server: str) -> str | None:
    channel = _reciprocal_ctx(channel, server)
    if channel is None:
        _dbg("_build_reciprocal_keyreq_on_accept: own handle unknown — skipping")
        return None
    with db_conn() as c:
        row = c.execute(
            "SELECT status FROM incoming WHERE handle = ? AND channel = ?",
            (sender_handle, channel),
        ).fetchone()
        c.execute(
            "DELETE FROM pending WHERE channel = ?",
            (_pending_key(channel, sender_handle),),
        )
    already_trusted = row is not None and row[0] == "trusted"
    if already_trusted:
        return None
    return build_keyreq(channel, sender_handle)


def handle_keyreq(server: str, sender_handle: str, nick: str, body: str) -> tuple[str | None, str | None]:
    _dbg(f"handle_keyreq entry server={server} sender={nick}!{sender_handle}")
    req = parse_keyreq(body)
    if req is None:
        _dbg("handle_keyreq: parse_keyreq returned None")
        return None, None
    if not _allow_incoming(sender_handle):
        _dbg(f"handle_keyreq: rate-limited incoming from {sender_handle}")
        return None, None
    sig_payload = _sig_payload_keyreq(
        req["channel"], req["pub"], req["eph_x25519"], req["nonce"]
    )
    if not ed25519_verify(req["pub"], sig_payload, req["sig"]):
        _dbg(f"handle_keyreq: sig verify failed for {sender_handle} on {req['channel']}")
        return None, None
    # The handshake `channel` field is the context key as the sender
    # understood it (channel name or `@<our_handle>` for PMs). We trust
    # that verbatim — the signature binds it.
    ctx = req["channel"]
    _dbg(f"handle_keyreq: parsed channel={ctx} pub={req['pub'].hex()[:16]}")
    with db_conn() as c:
        row = c.execute(
            "SELECT enabled, mode FROM channels WHERE channel = ?", (ctx,)
        ).fetchone()
    if row is None or not row[0]:
        _dbg(f"handle_keyreq: channel {ctx} not enabled (row={row})")
        return None, None
    mode = row[1] if row else "normal"
    peer_fp = fingerprint(req["pub"])
    change = _classify_peer_change(peer_fp, sender_handle)
    _dbg(f"handle_keyreq: channel={ctx} mode={mode} classify={change}")
    if change == "revoked":
        if weechat:
            weechat.prnt(
                "",
                f"{C_WARN}[E2E] WARNING: received KEYREQ from revoked peer {sender_handle}{C_RST}",
            )
        _record_pending_trust_change(
            sender_handle, ctx, "revoked", None, peer_fp, peer_fp
        )
        return None, None
    if change.startswith("handle_changed:"):
        old = change.split(":", 1)[1]
        if weechat:
            weechat.prnt(
                "",
                f"{C_WARN}[E2E] WARNING: known key {peer_fp.hex()[:16]} appeared under new handle — was {old}, now {sender_handle}{C_RST}",
            )
        _record_pending_trust_change(
            sender_handle, ctx, "handle_changed", None, peer_fp, peer_fp
        )
        return None, None
    if change.startswith("fingerprint_changed:"):
        old_fp_hex = change.split(":", 1)[1]
        old_fp_bytes = bytes.fromhex(old_fp_hex) if old_fp_hex else None
        if weechat:
            weechat.prnt(
                "",
                f"{C_ERR}[E2E] WARNING: fingerprint changed for {sender_handle} on {ctx} — old={old_fp_hex[:16] if old_fp_hex else '?'} new={peer_fp.hex()[:16]} — run /e2e reverify <nick>{C_RST}",
            )
        _record_pending_trust_change(
            sender_handle,
            ctx,
            "fingerprint_changed",
            req["pub"],
            old_fp_bytes,
            peer_fp,
        )
        return None, None
    with db_conn() as c:
        c.execute(
            "INSERT OR REPLACE INTO peers (fp, pk, last_handle, last_nick, first_seen, last_seen, status) VALUES (?, ?, ?, ?, ?, ?, ?)",
            (
                peer_fp,
                req["pub"],
                sender_handle,
                nick,
                int(time.time()),
                int(time.time()),
                "pending",
            ),
        )
    autotrust = _autotrust_matches(sender_handle, ctx)
    if autotrust:
        effective_mode = "auto-accept"
    else:
        effective_mode = mode
    with db_conn() as c:
        sess = c.execute(
            "SELECT status FROM incoming WHERE handle = ? AND channel = ?",
            (sender_handle, ctx),
        ).fetchone()
    already_trusted = sess is not None and sess[0] == "trusted"
    _dbg(
        f"handle_keyreq: effective_mode={effective_mode} already_trusted={already_trusted} "
        f"autotrust={autotrust} sess={sess}"
    )
    if effective_mode == "quiet" and not already_trusted:
        _dbg("handle_keyreq: quiet mode + not trusted → dropping")
        return None, None
    if effective_mode == "normal" and not already_trusted and not autotrust:
        with db_conn() as c:
            c.execute(
                "INSERT OR REPLACE INTO incoming (handle, channel, fp, sk, status, created_at) VALUES (?, ?, ?, ?, 'pending', ?)",
                (sender_handle, ctx, peer_fp, b"\x00" * 32, int(time.time())),
            )
            c.execute(
                "INSERT OR REPLACE INTO pending_inbound (handle, channel, sender_handle, pubkey, eph_x25519, nonce, sig, received_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    sender_handle,
                    ctx,
                    sender_handle,
                    req["pub"],
                    req["eph_x25519"],
                    req["nonce"],
                    req["sig"],
                    int(time.time()),
                ),
            )
        if weechat:
            buf = _notice_buffer_for_ctx(server, ctx, nick)
            who = f"{nick} ({sender_handle})" if nick else sender_handle
            weechat.prnt(
                buf,
                f"{C_WARN}[E2E] Pending key exchange from {who} for {ctx} — /e2e accept <nick>{C_RST}",
            )
        _dbg(f"handle_keyreq: normal-mode cached pending for {sender_handle} on {ctx}")
        return None, None
    _dbg(
        f"handle_keyreq: building KEYRSP for {sender_handle} on {ctx} "
        f"(effective_mode={effective_mode}, already_trusted={already_trusted})"
    )
    rsp = _build_keyrsp_for_req(
        req["channel"], sender_handle, req["pub"], req["eph_x25519"]
    )
    _dbg(f"handle_keyreq: _build_keyrsp_for_req returned rsp={'yes' if rsp else 'no'}")
    reciprocal = _maybe_build_reciprocal_keyreq(req["channel"], sender_handle, server)
    _dbg(f"handle_keyreq: reciprocal={'yes' if reciprocal else 'no'}")
    return rsp, reciprocal


def handle_keyrsp(server: str, nick: str, sender_handle: str, body: str) -> bool:
    _dbg(f"handle_keyrsp entry sender={sender_handle}")
    rsp = parse_keyrsp(body)
    if rsp is None:
        _dbg("handle_keyrsp: parse_keyrsp returned None")
        _prnt_dbg(server, "", nick, f"RX KEYRSP from {nick} ({sender_handle}) parse failed")
        return False
    ctx = rsp["channel"]
    _prnt_dbg(server, ctx, nick, f"RX KEYRSP from {nick} ({sender_handle}) for {ctx}")
    sig_payload = _sig_payload_keyrsp(
        ctx,
        rsp["pub"],
        rsp["eph_pub"],
        rsp["wrap_nonce"],
        rsp["wrap_ct"],
        rsp["nonce"],
    )
    if not ed25519_verify(rsp["pub"], sig_payload, rsp["sig"]):
        _dbg(f"handle_keyrsp: sig verify failed for {sender_handle} on {ctx}")
        _prnt_dbg(server, ctx, nick, f"KEYRSP from {nick} ({sender_handle}) failed on {ctx}: bad signature")
        return False
    with db_conn() as c:
        row = c.execute(
            "SELECT eph_sk FROM pending WHERE channel = ?",
            (_pending_key(ctx, sender_handle),),
        ).fetchone()
        if row is None:
            row = c.execute(
                "SELECT eph_sk FROM pending WHERE channel = ?", (ctx,)
            ).fetchone()
            if row is None:
                _dbg(f"handle_keyrsp: NO pending outgoing KEYREQ for channel {ctx} sender={sender_handle} — dropping")
                _prnt_dbg(server, ctx, nick, f"KEYRSP from {nick} ({sender_handle}) dropped on {ctx}: no pending KEYREQ")
                return False
            c.execute("DELETE FROM pending WHERE channel = ?", (ctx,))
        else:
            c.execute(
                "DELETE FROM pending WHERE channel = ?",
                (_pending_key(ctx, sender_handle),),
            )
        eph_sk = row[0]
    _dbg(f"handle_keyrsp: consumed pending entry for channel {ctx}")
    shared = x25519_ecdh(eph_sk, rsp["eph_pub"])
    info = b"RPE2E01-WRAP:" + ctx.encode()
    wrap_key = hkdf_sha256(HKDF_SALT, shared, info, KEY_LEN)
    session_key = aead_decrypt(wrap_key, rsp["wrap_nonce"], info, rsp["wrap_ct"])
    if session_key is None or len(session_key) != KEY_LEN:
        _prnt_dbg(server, ctx, nick, f"KEYRSP from {nick} ({sender_handle}) failed on {ctx}: wrap decrypt failed")
        return False
    peer_fp = fingerprint(rsp["pub"])
    change = _classify_peer_change(peer_fp, sender_handle)
    if change == "revoked":
        if weechat:
            weechat.prnt(
                "",
                f"{C_WARN}[E2E] WARNING: received key from revoked peer {sender_handle}{C_RST}",
            )
        _record_pending_trust_change(
            sender_handle, ctx, "revoked", None, peer_fp, peer_fp
        )
        _prnt_dbg(server, ctx, nick, f"KEYRSP from {nick} ({sender_handle}) rejected on {ctx}: revoked peer")
        return False
    if change.startswith("handle_changed:"):
        old = change.split(":", 1)[1]
        if weechat:
            weechat.prnt(
                "",
                f"{C_WARN}[E2E] WARNING: known key {peer_fp.hex()[:16]} appeared under new handle — was {old}, now {sender_handle}{C_RST}",
            )
        _record_pending_trust_change(
            sender_handle, ctx, "handle_changed", None, peer_fp, peer_fp
        )
        _prnt_dbg(server, ctx, nick, f"KEYRSP from {nick} ({sender_handle}) rejected on {ctx}: handle changed from {old}")
        return False
    if change.startswith("fingerprint_changed:"):
        old_fp_hex = change.split(":", 1)[1]
        old_fp_bytes = bytes.fromhex(old_fp_hex) if old_fp_hex else None
        if weechat:
            weechat.prnt(
                "",
                f"{C_ERR}[E2E] WARNING: fingerprint changed for {sender_handle} on {ctx} — run /e2e reverify <nick>{C_RST}",
            )
        _record_pending_trust_change(
            sender_handle,
            ctx,
            "fingerprint_changed",
            rsp["pub"],
            old_fp_bytes,
            peer_fp,
        )
        _prnt_dbg(server, ctx, nick, f"KEYRSP from {nick} ({sender_handle}) rejected on {ctx}: fingerprint changed")
        return False
    now = int(time.time())
    with db_conn() as c:
        existing = c.execute(
            "SELECT fp FROM incoming WHERE handle = ? AND channel = ?",
            (sender_handle, ctx),
        ).fetchone()
        if existing is not None and existing[0] != peer_fp:
            if weechat:
                weechat.prnt(
                    "",
                    f"{C_ERR}[E2E] WARNING: fingerprint mismatch for {sender_handle} on {ctx} — run /e2e reverify <nick>{C_RST}",
                )
            _record_pending_trust_change(
                sender_handle,
                ctx,
                "fingerprint_changed",
                rsp["pub"],
                existing[0],
                peer_fp,
            )
            _dbg(f"handle_keyrsp: fingerprint mismatch for {sender_handle} on {ctx}")
            _prnt_dbg(server, ctx, nick, f"KEYRSP from {nick} ({sender_handle}) rejected on {ctx}: incoming fingerprint mismatch")
            return False
        c.execute(
            "INSERT OR REPLACE INTO peers (fp, pk, last_handle, last_nick, first_seen, last_seen, status) VALUES (?, ?, ?, ?, ?, ?, 'trusted')",
            (peer_fp, rsp["pub"], sender_handle, None, now, now),
        )
        c.execute(
            "INSERT OR REPLACE INTO incoming (handle, channel, fp, sk, status, created_at) VALUES (?, ?, ?, ?, 'trusted', ?)",
            (sender_handle, ctx, peer_fp, session_key, now),
        )
    _dbg(
        f"handle_keyrsp: installed trusted incoming for {sender_handle} on {ctx} "
        f"fp={peer_fp.hex()[:16]}"
    )
    _prnt_dbg(server, ctx, nick, f"KEYRSP from {nick} ({sender_handle}) installed session on {ctx}")
    return True


def handle_rekey(sender_handle: str, nick: str, body: str) -> bool:
    rk = parse_keyrekey(body)
    if rk is None:
        return False
    sig_payload = _sig_payload_keyrekey(
        rk["channel"],
        rk["pub"],
        rk["eph_pub"],
        rk["wrap_nonce"],
        rk["wrap_ct"],
        rk["nonce"],
    )
    if not ed25519_verify(rk["pub"], sig_payload, rk["sig"]):
        return False
    ctx = rk["channel"]
    peer_fp = fingerprint(rk["pub"])
    change = _classify_peer_change(peer_fp, sender_handle)
    if change == "new":
        if weechat:
            weechat.prnt(
                "",
                f"{C_WARN}[E2E] WARNING: unsolicited REKEY from unknown peer {sender_handle} — ignoring{C_RST}",
            )
        return False
    if change == "revoked":
        if weechat:
            weechat.prnt(
                "",
                f"{C_WARN}[E2E] WARNING: received REKEY from revoked peer {sender_handle}{C_RST}",
            )
        _record_pending_trust_change(
            sender_handle, ctx, "revoked", None, peer_fp, peer_fp
        )
        return False
    if change.startswith("handle_changed:"):
        old = change.split(":", 1)[1]
        if weechat:
            weechat.prnt(
                "",
                f"{C_WARN}[E2E] WARNING: known key {peer_fp.hex()[:16]} appeared under new handle — was {old}, now {sender_handle}{C_RST}",
            )
        _record_pending_trust_change(
            sender_handle, ctx, "handle_changed", None, peer_fp, peer_fp
        )
        return False
    if change.startswith("fingerprint_changed:"):
        old_fp_hex = change.split(":", 1)[1]
        old_fp_bytes = bytes.fromhex(old_fp_hex) if old_fp_hex else None
        if weechat:
            weechat.prnt(
                "",
                f"{C_ERR}[E2E] WARNING: fingerprint changed for {sender_handle} on {ctx} — run /e2e reverify <nick>{C_RST}",
            )
        _record_pending_trust_change(
            sender_handle,
            ctx,
            "fingerprint_changed",
            rk["pub"],
            old_fp_bytes,
            peer_fp,
        )
        return False
    pk, sk, _fp = ensure_identity()
    my_x25519_scalar = ed25519_sk_to_x25519_scalar(sk, pk)
    shared = x25519_ecdh(my_x25519_scalar, rk["eph_pub"])
    info = b"RPE2E01-REKEY:" + ctx.encode()
    wrap_key = hkdf_sha256(HKDF_SALT, shared, info, KEY_LEN)
    session_key = aead_decrypt(wrap_key, rk["wrap_nonce"], info, rk["wrap_ct"])
    if session_key is None or len(session_key) != KEY_LEN:
        return False
    now = int(time.time())
    with db_conn() as c:
        existing = c.execute(
            "SELECT fp FROM incoming WHERE handle = ? AND channel = ?",
            (sender_handle, ctx),
        ).fetchone()
        if existing is not None and existing[0] != peer_fp:
            if weechat:
                weechat.prnt(
                    "",
                    f"{C_ERR}[E2E] WARNING: fingerprint mismatch for {sender_handle} on {ctx} — run /e2e reverify <nick>{C_RST}",
                )
            _record_pending_trust_change(
                sender_handle,
                ctx,
                "fingerprint_changed",
                rk["pub"],
                existing[0],
                peer_fp,
            )
            return False
        c.execute(
            "INSERT OR REPLACE INTO incoming (handle, channel, fp, sk, status, created_at) VALUES (?, ?, ?, ?, 'trusted', ?)",
            (sender_handle, ctx, peer_fp, session_key, now),
        )
    return True


def _build_rekey_for_peer(
    channel: str, peer_handle: str, peer_pk: bytes, new_sk: bytes
) -> str:
    pk, sk, _fp = ensure_identity()
    eph_sk, eph_pk = generate_x25519_keypair()
    peer_x25519 = ed25519_pk_to_x25519(peer_pk)
    shared = x25519_ecdh(eph_sk, peer_x25519)
    info = b"RPE2E01-REKEY:" + channel.encode()
    wrap_key = hkdf_sha256(HKDF_SALT, shared, info, KEY_LEN)
    wrap_nonce, wrap_ct = aead_encrypt(wrap_key, info, new_sk)
    nonce = nacl_random(16)
    sig_payload = _sig_payload_keyrekey(channel, pk, eph_pk, wrap_nonce, wrap_ct, nonce)
    sig = ed25519_sign(sk, sig_payload)
    body = (
        f"{CTCP_TAG} REKEY v=1 c={channel} p={_b64u_encode(pk)} "
        f"e={_b64u_encode(eph_pk)} wn={_b64u_encode(wrap_nonce)} "
        f"w={_b64u_encode(wrap_ct)} "
        f"n={_b64u_encode(nonce)} s={_b64u_encode(sig)}"
    )
    return "\x01" + body + "\x01"


def _distribute_rekey(channel: str, new_sk: bytes, server: str | None = None) -> None:
    with db_conn() as c:
        recipients = c.execute(
            "SELECT handle, fingerprint FROM outgoing_recipients WHERE channel = ?",
            (channel,),
        ).fetchall()
    for handle, fp_bytes in recipients:
        with db_conn() as c2:
            peer_row = c2.execute(
                "SELECT pk, last_nick FROM peers WHERE fp = ?", (fp_bytes,)
            ).fetchone()
        if peer_row is None:
            continue
        peer_pk = peer_row[0]
        last_nick = peer_row[1]
        ctcp = _build_rekey_for_peer(channel, handle, peer_pk, new_sk)
        # Prefer the stored last_nick over parsing the handle (which is
        # an `ident@host`, not a nick) so we /quote NOTICE to something
        # the IRC server will route.
        nick = last_nick or (handle.split("@")[0] if "@" in handle else handle)
        if weechat:
            if server:
                _send_raw_notice(server, nick, ctcp)
            else:
                # No server context — last resort, send on the
                # currently-active buffer. Not great, but this branch
                # is only reached when rekey distribution fires from
                # a context that didn't carry a server string.
                weechat.command("", f"/quote NOTICE {nick} :{ctcp}")


def parse_wire(line: str) -> dict | None:
    if not line.startswith(WIRE_PREFIX):
        return None
    try:
        parts = line.split(" ", 4)
        if len(parts) != 5 or parts[0] != WIRE_PREFIX:
            return None
        msgid_hex, ts_s, parttot, body = parts[1], parts[2], parts[3], parts[4]
        if len(msgid_hex) != 16:
            return None
        part_s, total_s = parttot.split("/", 1)
        part, total = int(part_s), int(total_s)
        if total < 1 or total > MAX_CHUNKS or part < 1 or part > total:
            return None
        nonce_b64, ct_b64 = body.split(":", 1)
        nonce = base64.b64decode(nonce_b64)
        if len(nonce) != NONCE_LEN:
            return None
        ct = base64.b64decode(ct_b64)
        return {
            "msgid": bytes.fromhex(msgid_hex),
            "ts": int(ts_s),
            "part": part,
            "total": total,
            "nonce": nonce,
            "ct": ct,
        }
    except Exception:
        return None


def encode_wire(
    msgid: bytes, ts: int, part: int, total: int, nonce: bytes, ct: bytes
) -> str:
    return (
        f"{WIRE_PREFIX} {msgid.hex()} {ts} {part}/{total} "
        f"{base64.b64encode(nonce).decode()}:{base64.b64encode(ct).decode()}"
    )


def split_plaintext(pt: str) -> list[bytes]:
    return split_plaintext_budget(pt, MAX_PT_PER_CHUNK)


def split_plaintext_budget(pt: str, max_per_chunk: int) -> list[bytes]:
    """`split_plaintext` with a caller-chosen per-chunk byte budget — mirrors
    Rust `src/e2e/chunker.rs::split_plaintext_budget`. Used by the CTCP ACTION
    splitter, whose per-piece budget must leave room for the `\x01ACTION …\x01`
    framing each piece is wrapped in afterwards. The `MAX_CHUNKS` cap applies to
    the produced pieces regardless of budget."""
    # G13: refuse empty plaintext outright — mirrors Rust
    # `src/e2e/chunker.rs::split_plaintext`. No zero-length-ciphertext
    # chunk should ever be shipped to a peer.
    if not pt:
        raise ValueError("empty plaintext")
    b = pt.encode("utf-8")
    chunks: list[bytes] = []
    i = 0
    while i < len(b):
        j = min(i + max_per_chunk, len(b))
        while j > i and j < len(b) and (b[j] & 0xC0) == 0x80:
            j -= 1
        if j == i:
            raise ValueError("cannot split: UTF-8 codepoint too large")
        chunks.append(b[i:j])
        i = j
        if len(chunks) > MAX_CHUNKS:
            raise ValueError(f"chunk limit: {len(chunks)} > {MAX_CHUNKS}")
    return chunks


def _get_or_generate_outgoing_key(channel: str) -> bytes:
    with db_conn() as c:
        row = c.execute(
            "SELECT sk, pending_rotation FROM outgoing WHERE channel = ?", (channel,)
        ).fetchone()
        if row is not None and not row[1]:
            return row[0]
        fresh = nacl_random(KEY_LEN)
        c.execute(
            "INSERT OR REPLACE INTO outgoing VALUES (?, ?, ?, 0)",
            (channel, fresh, int(time.time())),
        )
        return fresh


def _get_or_generate_outgoing_key_with_rotation(channel: str) -> tuple[bytes, bool]:
    with db_conn() as c:
        row = c.execute(
            "SELECT sk, pending_rotation FROM outgoing WHERE channel = ?", (channel,)
        ).fetchone()
        if row is not None and not row[1]:
            return row[0], False
        had_pending = row is not None and row[1]
        fresh = nacl_random(KEY_LEN)
        c.execute(
            "INSERT OR REPLACE INTO outgoing VALUES (?, ?, ?, 0)",
            (channel, fresh, int(time.time())),
        )
        return fresh, had_pending


# ── Outbound E2E gate (F1) ──────────────────────────────────────────────
#
# Mirror of the Rust client's single fail-closed chokepoint
# (`src/app/e2e_gate.rs::e2e_encrypt_or_passthrough`). In the Rust client
# EVERY send path routes through that one gate. WeeChat has no equivalent
# high-level chokepoint, so we hook the low-level `irc_out1_privmsg` modifier:
# every PRIVMSG that reaches the wire — from plain input, `/msg`, `/say`,
# `/me` (ACTION), `/amsg`, … — passes through here regardless of the command
# that produced it. Without this, `/me`/`/msg`/`/say` bypassed
# `hook_input_text_for_buffer` (which bails on any `/`-command) and left an
# E2E conversation in PLAINTEXT.
#
# The gate NEVER downgrades an E2E-enabled conversation to plaintext: an
# unresolved handle, a keyring read error, or an encrypt failure all REFUSE
# (drop the line + show a visible `[E2E]` reason), exactly like the Rust
# `E2eRefusal` variants.

# Themed refusal lines (mirror `E2eRefusal::user_message`).
_REFUSE_NO_HANDLE = (
    "cannot encrypt PM without peer handle — wait for a message from them first"
)
_REFUSE_KEYRING = (
    "cannot encrypt — keyring read failed; message NOT sent (E2E stays on)"
)
_REFUSE_ENCRYPT = (
    "encryption failed — message NOT sent as plaintext (use /e2e off to send cleartext)"
)
_REFUSE_AMBIGUOUS = (
    "ambiguous target %s — E2E is enabled on both %s "
    "(STATUSMSG subset vs distinct channel); refusing to guess a context"
)

# `_config_enabled` result sentinels — explicit strings, never bare bools, so
# a read error can NEVER be mistaken for "not enabled" (that would fail open).
_ENABLED_YES = "yes"
_ENABLED_NO = "no"
_ENABLED_ERR = "err"


def _config_enabled(context: str) -> str:
    """Fail-closed enabled check: `_ENABLED_YES` / `_ENABLED_NO` / `_ENABLED_ERR`.
    A DB read error returns `_ENABLED_ERR` so the caller refuses rather than
    risk plaintext — mirrors the Rust gate's `KeyringRead` refusal."""
    try:
        with db_conn() as c:
            row = c.execute(
                "SELECT enabled FROM channels WHERE channel = ?", (context,)
            ).fetchone()
        return _ENABLED_YES if (row is not None and row[0]) else _ENABLED_NO
    except Exception as e:
        _dbg(f"_config_enabled read error for {context}: {e}")
        return _ENABLED_ERR


def _parse_out_privmsg(raw: str) -> tuple[str, str] | None:
    """Parse an `irc_out1_privmsg` line into `(target, body)`. Returns None if
    it is not a PRIVMSG we recognize (then the caller passes it through)."""
    s = raw
    if s.startswith("@"):  # strip IRCv3 message tags
        sp = s.find(" ")
        if sp == -1:
            return None
        s = s[sp + 1 :]
    if s.startswith(":"):  # strip an (unusual for client→server) source prefix
        sp = s.find(" ")
        if sp == -1:
            return None
        s = s[sp + 1 :]
    parts = s.split(" ", 2)
    if len(parts) < 3 or parts[0].upper() != "PRIVMSG":
        return None
    target = parts[1]
    body = parts[2]
    if body.startswith(":"):
        body = body[1:]
    return (target, body)


def _encrypt_plain_wire(sk: bytes, context: str, plain: str) -> list[str]:
    """Encrypt `plain` under session key `sk` for `context`, one wire line per
    chunk. `context` is the wire context used for the AAD (unscoped in F1)."""
    chunks = split_plaintext(plain)
    total = len(chunks)
    msgid = nacl_random(8)
    ts = int(time.time())
    out: list[str] = []
    for idx, chunk in enumerate(chunks, start=1):
        aad = build_aad(context, msgid, ts, idx, total)
        nonce, ct = aead_encrypt(sk, aad, chunk)
        out.append(encode_wire(msgid, ts, idx, total, nonce, ct))
    return out


def _encrypt_ctcp_wire(sk: bytes, context: str, frame: str) -> list[str]:
    """Encrypt a `\x01ACTION …\x01` CTCP frame — mirror of Rust
    `E2eManager::encrypt_outgoing_ctcp`. A frame fitting one chunk encrypts
    as-is; a longer ACTION splits into independent, individually-wrapped
    `\x01ACTION piece\x01` frames (never fragmenting the CTCP envelope across
    bare chunks). Byte length, not char length, gates the one-chunk case."""
    action_prefix = "\x01ACTION "
    action_framing = len(action_prefix) + 1  # +1 for the trailing \x01
    if len(frame.encode("utf-8")) <= MAX_PT_PER_CHUNK:
        return _encrypt_plain_wire(sk, context, frame)
    if not (frame.startswith(action_prefix) and frame.endswith("\x01")):
        raise ValueError("CTCP frame exceeds one encrypted chunk and cannot be split")
    body = frame[len(action_prefix) : -1]
    budget = MAX_PT_PER_CHUNK - action_framing
    pieces = split_plaintext_budget(body, budget)
    out: list[str] = []
    for piece in pieces:
        piece_str = piece.decode("utf-8")
        out.extend(_encrypt_plain_wire(sk, context, f"{action_prefix}{piece_str}\x01"))
    return out


def _distribute_rekey_safe(channel: str, new_sk: bytes, server: str) -> None:
    """Best-effort REKEY distribution that never raises. A distribution failure
    must NOT refuse the user's message: the outgoing key has already rotated
    (so a revoked peer is excluded regardless), and a trusted peer that misses
    the REKEY re-handshakes on its next undecryptable ciphertext (auto-KEYREQ).
    Any hard failure is logged; per-peer failures already warn inside
    `_distribute_rekey`."""
    try:
        _distribute_rekey(channel, new_sk, server)
    except Exception as e:
        _dbg(f"_distribute_rekey_safe: distribution failed for {channel}: {e}")


def _channel_readings(target: str) -> list:
    """Every channel reading of a possibly-STATUSMSG-prefixed target, most-
    stripped first; empty list → not a channel. `@#chan` has exactly one
    reading (#chan): a message to a channel subset is still that channel for
    E2E, and without stripping, `/msg @#secret …` classified as a DM to a
    bogus nick and leaked plaintext to the channel ops on an E2E-enabled
    channel. But `+` and `&` are BOTH status prefixes AND channel prefixes
    (RFC 2811), so `+#secret` is genuinely ambiguous without per-server
    ISUPPORT: the voiced subset of #secret OR the distinct channel "+#secret".
    Callers get every reading and must stay fail-closed across all of them —
    encrypting under the one enabled reading is always safe (a peer on the
    other reading sees undecryptable ciphertext, never plaintext). Mirrors the
    Perl helper of the same name."""
    run = 0
    while run < len(target) and target[run] in "@%+&~":
        run += 1
    readings = []
    for j in range(run, -1, -1):
        rest = target[j:]
        if rest and rest[0] in CHANNEL_PREFIXES:
            readings.append(rest)
    return readings


def _e2e_gate_wire(server: str, target: str, body: str):
    """Fail-closed outbound gate — mirror of `e2e_encrypt_or_passthrough`.

    Returns one of:
      ("pass", None)          — send `body` unchanged (no E2E state / bypass)
      ("cipher", [wire, …])   — send these encrypted lines, drop the original
      ("refuse", themed_msg)  — drop; caller shows the `[E2E]` reason
      ("bypass", warn_or_None)— channel bot bypass; send plaintext, maybe warn

    Never raises: any unexpected error resolves to a refusal, because at the
    point it can be reached we can no longer rule out an enabled E2E context
    and must not leak plaintext."""
    try:
        # STATUSMSG targets (`@#chan`) address a channel subset — classify and
        # key them by the underlying channel, never as a DM. `+#chan`/`&#chan`
        # are ambiguous (see _channel_readings): every reading is considered.
        readings = _channel_readings(target)
        is_channel = bool(readings)
        chan = readings[0] if readings else target

        # Non-ACTION CTCP (VERSION, PING, …) is a deliberate plaintext escape
        # hatch, exactly as the Rust client leaves /ctcp and /version in
        # cleartext. Only ACTION frames and plain text are encrypted.
        is_ctcp = len(body) >= 2 and body.startswith("\x01") and body.endswith("\x01")
        is_action = body.startswith("\x01ACTION ") and body.endswith("\x01")
        if is_ctcp and not is_action:
            return ("pass", None)

        # Bot-command bypass: `.cmd`/`!cmd` go out unencrypted so channel bots
        # can parse them. CHANNEL-ONLY and SINGLE-LINE (a newline forces the
        # full gate); DMs never bypass. Mirror of the channel-only bypass +
        # `warn_e2e_bot_bypass` visibility in the Rust gate.
        if is_channel and (body.startswith(".") or body.startswith("!")) and "\n" not in body:
            # Warn whenever we cannot POSITIVELY confirm the channel is
            # non-E2E: an enabled config OR a keyring read error both mean the
            # cleartext bypass may be surprising, so the warning must show
            # (fail-closed visibility — never silently drop the warning on a
            # transient DB error).
            warn = None
            if any(_config_enabled(r) != _ENABLED_NO for r in readings):
                warn = (
                    f"bot-style message to {chan} sent in CLEARTEXT — channel "
                    "lines starting with '.' or '!' bypass encryption for bots"
                )
            return ("bypass", warn)

        # Resolve the keyring context (unscoped in F1; F3 adds network scope).
        if is_channel:
            # Encrypt under whichever reading has E2E enabled — a peer on the
            # other reading of an ambiguous target sees ciphertext, never
            # plaintext. Two enabled readings cannot be disambiguated locally
            # → refuse; a read error on ANY reading refuses too (fail-closed).
            states = [(r, _config_enabled(r)) for r in readings]
            if any(s == _ENABLED_ERR for _, s in states):
                return ("refuse", _REFUSE_KEYRING)
            on = [r for r, s in states if s == _ENABLED_YES]
            if len(on) > 1:
                return ("refuse", _REFUSE_AMBIGUOUS % (target, " and ".join(on)))
            context = on[0] if on else chan
        else:
            # Live OR cached handle resolution (mirror of Rust
            # `resolve_query_peer_handle`): a peer who isn't currently in a
            # shared nicklist is still resolvable from the persisted peers
            # cache, so the `@<handle>` config the DM was enabled under is
            # found instead of being missed into a plaintext send.
            handle = _resolve_handle_for_command(server, target, target)
            if handle is None:
                # Nothing resolvable → no `@<handle>` config can exist for this
                # nick (any handshake would have cached a handle). Refuse only
                # if an enabled legacy bare-nick row exists; otherwise plaintext
                # passthrough is safe (no E2E state). Fail closed on a read err.
                legacy = _config_enabled(target)
                if legacy == _ENABLED_ERR:
                    return ("refuse", _REFUSE_KEYRING)
                if legacy == _ENABLED_YES:
                    return ("refuse", _REFUSE_NO_HANDLE)
                return ("pass", None)
            context = "@" + handle

        enabled = _config_enabled(context)
        if enabled == _ENABLED_ERR:
            return ("refuse", _REFUSE_KEYRING)
        if enabled == _ENABLED_NO:
            return ("pass", None)

        # Enabled — encrypt. A key-generation or encrypt failure REFUSES
        # (never plaintext). REKEY distribution is best-effort and must NOT
        # refuse the message: the key has already rotated (the revoked peer is
        # excluded regardless), and any trusted peer that misses the REKEY
        # re-handshakes on its next undecryptable ciphertext via auto-KEYREQ —
        # mirroring the Rust gate, whose REKEY NOTICEs are queued/retried
        # rather than gating the send.
        try:
            fresh, had_pending = _get_or_generate_outgoing_key_with_rotation(context)
        except Exception as e:
            _dbg(f"_e2e_gate_wire key generation failed for {context}: {e}")
            return ("refuse", _REFUSE_ENCRYPT)
        if had_pending:
            _distribute_rekey_safe(context, fresh, server)
        try:
            if is_action:
                wires = _encrypt_ctcp_wire(fresh, context, body)
            else:
                wires = _encrypt_plain_wire(fresh, context, body)
        except Exception as e:
            _dbg(f"_e2e_gate_wire encrypt failed for {context}: {e}")
            return ("refuse", _REFUSE_ENCRYPT)
        return ("cipher", wires)
    except Exception as e:
        _dbg(f"_e2e_gate_wire OUTER EXCEPTION for {target}: {e}\n{traceback.format_exc()}")
        # We touched keyring state but failed before ruling E2E out — refuse
        # rather than risk plaintext (fail-closed).
        return ("refuse", _REFUSE_KEYRING)


def _target_buffer(server: str, target: str) -> str:
    """Best-effort buffer pointer for showing an `[E2E]` line about `target`."""
    if weechat is None:
        return ""
    buf = weechat.buffer_search("irc", f"{server}.{target}") or ""
    if not buf:
        buf = weechat.buffer_search("irc", f"server.{server}") or ""
    return buf


def hook_irc_out_privmsg(data, modifier, server, msg):
    """`irc_out1_privmsg` modifier — the authoritative fail-closed outbound
    gate. Returns the (possibly unchanged) line, or "" to drop it after having
    sent encrypted chunks / refused."""
    try:
        if weechat is None:
            return msg
        parsed = _parse_out_privmsg(msg)
        if parsed is None:
            return msg
        target, body = parsed
        # Re-entrancy guard: our own `_send_raw_privmsg` re-emits ciphertext
        # through this modifier — never re-process an already-encrypted line.
        # Key the guard on a STRUCTURALLY VALID wire chunk (parse_wire), NOT a
        # bare `+RPE2E01` prefix: a user command like `/msg bob +RPE2E01 secret`
        # starts with the prefix but is not a real chunk, and must still go
        # through the gate (else it would leak as plaintext to an E2E DM). A
        # crafted string that DOES parse as a chunk carries no readable
        # plaintext, so passing it through is harmless.
        if parse_wire(body) is not None:
            return msg
    except Exception as e:
        # A line we cannot even parse carries no derivable E2E context; the
        # irc_out1_privmsg feed is always a PRIVMSG, so this is unreachable in
        # practice. Passing it through cannot leak an E2E conversation.
        _dbg(f"hook_irc_out_privmsg parse EXCEPTION: {e}")
        return msg

    action, payload = _e2e_gate_wire(server, target, body)
    if action == "pass":
        return msg
    if action == "bypass":
        if payload:
            _prnt_warn(_target_buffer(server, target), payload)
        return msg
    if action == "refuse":
        # For command paths (/msg, /me, /say) weechat's core may already have
        # echoed the plaintext locally, so make the drop explicit.
        _prnt_err(
            _target_buffer(server, target),
            f"{payload} — the message shown above was NOT delivered",
        )
        _dbg(f"hook_irc_out_privmsg REFUSED send to {target}: {payload}")
        return ""
    # action == "cipher": send the encrypted chunks ourselves and drop the
    # original. Once we have decided to encrypt we NEVER fall back to
    # plaintext, even if a send raises mid-loop — but the failure must be
    # VISIBLE (never a silent message drop).
    try:
        for line in payload:
            _send_raw_privmsg(server, target, line)
    except Exception as e:
        _dbg(f"hook_irc_out_privmsg send EXCEPTION for {target}: {e}")
        _prnt_err(
            _target_buffer(server, target),
            f"failed to send encrypted message to {target} — NOT delivered "
            "(message stays encrypted; retry when the connection recovers)",
        )
    return ""


def hook_signal_server_connected(data, signal, signal_data):
    """Registration complete: RESET the own handle (the server may assign a
    different ident/host/cloak this session) and re-seed it with the one-shot
    self-USERHOST. `signal_data` is the server name."""
    try:
        server = signal_data or ""
        _own_handle.pop(server, None)
        _send_self_userhost(server)
    except Exception as e:
        _dbg(f"hook_signal_server_connected: {e}")
    return weechat.WEECHAT_RC_OK if weechat else 0


def hook_signal_server_disconnected(data, signal, signal_data):
    try:
        _own_handle.pop(signal_data or "", None)
    except Exception as e:
        _dbg(f"hook_signal_server_disconnected: {e}")
    return weechat.WEECHAT_RC_OK if weechat else 0


def hook_irc_in_302(data, modifier, server, msg):
    """RPL_USERHOST: seed our own handle from any entry matching our nick
    (the self-USERHOST on connect, or any later USERHOST that includes us).
    Rank 1: solanum-family ircds (Libera) answer a SELF-query with the REAL
    host, not the cloak peers see — so this only fills a hole and never
    overrides a prefix-visible source. Observe-only, line passes through."""
    try:
        trailing = msg.split(" :", 1)[1] if " :" in msg else msg.rsplit(" ", 1)[-1]
        own = _own_nick(server)
        for entry in trailing.split():
            parsed = _parse_userhost_reply(entry)
            if parsed and own and parsed[0].lower() == own.lower():
                _set_own_handle(server, parsed[1], rank=1)
    except Exception as e:
        _dbg(f"hook_irc_in_302: {e}")
    return msg


def hook_irc_in_join(data, modifier, server, msg):
    """Our own JOIN (`:nick!ident@host JOIN :#chan`) carries our prefix as
    peers see it — the strongest own-handle source next to echo-message, and
    present right after connect (autojoin). Observe-only."""
    try:
        if msg.startswith(":"):
            prefix = msg[1:].split(" ", 1)[0]
            if "!" in prefix and "@" in prefix:
                nick, userhost = prefix.split("!", 1)
                own = _own_nick(server)
                if own and nick.lower() == own.lower():
                    _set_own_handle(server, userhost)
    except Exception as e:
        _dbg(f"hook_irc_in_join: {e}")
    return msg


def hook_irc_in_396(data, modifier, server, msg):
    """RPL_HOSTHIDDEN: `:srv 396 nick <host|user@host> :is now your ...` —
    the server telling US our new DISPLAYED host (peer-visible by
    definition). Host-only form merges with the known ident. Observe-only."""
    try:
        parts = msg.split(" ")
        if len(parts) >= 4:
            newhost = parts[3].lstrip(":")
            # mirror irssi core's sanity check on the announced host
            if newhost and not any(c in newhost for c in "*?!# ") and newhost[0] not in "@:-" and not newhost.endswith("-"):
                if "@" in newhost:
                    _set_own_handle(server, newhost)
                else:
                    cur = _own_handle_get(server)
                    if cur and "@" in cur:
                        _set_own_handle(server, cur.split("@", 1)[0] + "@" + newhost)
    except Exception as e:
        _dbg(f"hook_irc_in_396: {e}")
    return msg


def hook_irc_in_chghost(data, modifier, server, msg):
    """CHGHOST `:nick!old@old CHGHOST newident newhost`: track our OWN handle
    (peer handles are always read live from prefixes/nicklists). weechat's
    irc plugin only forwards CHGHOST when the cap is active, so this is a
    no-op otherwise. Observe-only."""
    try:
        if msg.startswith(":"):
            parts = msg.split(" ")
            nick = parts[0][1:].split("!", 1)[0]
            own = _own_nick(server)
            if own and nick.lower() == own.lower() and len(parts) >= 4:
                newuser = parts[2]
                newhost = parts[3].lstrip(":")
                if newuser and newhost:
                    _set_own_handle(server, f"{newuser}@{newhost}")
    except Exception as e:
        _dbg(f"hook_irc_in_chghost: {e}")
    return msg


def hook_irc_in_privmsg(data, modifier, server, msg):
    try:
        if not msg.startswith(":"):
            return msg
        prefix_end = msg.index(" ")
        prefix = msg[1:prefix_end]
        rest = msg[prefix_end + 1 :]
        if "!" not in prefix or "@" not in prefix:
            return msg
        nick, userhost = prefix.split("!", 1)
        handle = userhost
        rest_parts = rest.split(" ", 2)
        if len(rest_parts) < 3 or rest_parts[0] != "PRIVMSG":
            return msg
        target = rest_parts[1]
        text = rest_parts[2][1:] if rest_parts[2].startswith(":") else rest_parts[2]

        # echo-message: the server echoes our own PRIVMSG back with our
        # CANONICAL prefix — the freshest own-handle source there is (weechat
        # requests available caps by default, so this is commonly active).
        own = _own_nick(server)
        is_own_echo = bool(own) and nick.lower() == own.lower()
        if is_own_echo:
            _set_own_handle(server, handle)

        wire = parse_wire(text)
        if wire is None:
            return msg
        if is_own_echo:
            # Our own ciphertext echo: the plaintext was already rendered
            # locally at send time. Swallow it — falling through would treat
            # it as a peer's wire (decrypt fail → auto-KEYREQ to ourselves).
            return ""
        _dbg(
            f"hook_irc_in_privmsg RPE2E wire from {nick}!{handle} → {target} "
            f"msgid={wire['msgid'].hex() if isinstance(wire.get('msgid'), bytes) else wire.get('msgid')} "
            f"part={wire['part']}/{wire['total']}"
        )
        if wire["total"] > MAX_CHUNKS:
            return ""
        skew = abs(int(time.time()) - wire["ts"])
        if skew > TS_TOLERANCE:
            return ""
        # STATUSMSG delivery (`@#chan`): the decrypt context is the underlying
        # channel, mirroring the outbound gate. Keep the original `target` for
        # the reconstructed PRIVMSG line so weechat routes it unchanged; only
        # the context key is re-read. Without this, ciphertext sent to `@#chan`
        # would be keyed as a DM (`@<handle>`) and fail to decrypt (drop +
        # spurious KEYREQ). An ambiguous `+#chan`/`&#chan` target has several
        # readings: prefer one holding a trusted session for this sender (a
        # wrong pick cannot leak — AEAD decrypt just fails); the most-stripped
        # reading stays the default so auto-KEYREQ keeps keying off it.
        readings = _channel_readings(target)
        if readings:
            ctx_candidates = [context_key(r, handle) for r in readings]
        else:
            # DM: recipient-keyed context (docs/rpe2e-dm-addendum.md) — WE are
            # the recipient, so the context is OUR handle, never the sender's.
            own_h = _own_handle_get(server)
            if not own_h:
                # Own handle not learned yet (e.g. right after connect, before
                # the self-USERHOST reply). Do NOT fall back to `@<sender>` —
                # decrypting or KEYREQ-ing under it would negotiate the WRONG
                # DM direction. Drop; the peer's next message re-establishes
                # once the handle is known.
                now_f = time.time()
                wait_key = (server, nick)
                if now_f - _own_wait_notice_at.get(wait_key, 0.0) >= KEYREQ_MIN_INTERVAL:
                    _own_wait_notice_at[wait_key] = now_f
                    buf = weechat.buffer_search("irc", f"{server}.{nick}") if weechat else ""
                    _prnt_warn(
                        buf,
                        f"encrypted DM from {nick} held — own identity not "
                        "learned yet (waiting for the USERHOST reply)",
                    )
                _dbg(f"hook_irc_in_privmsg: DM wire from {nick} but own handle unknown on {server}")
                return ""
            ctx_candidates = ["@" + own_h]
        ctx = ctx_candidates[0]
        with db_conn() as c:
            row = c.execute(
                "SELECT sk, status FROM incoming WHERE handle = ? AND channel = ?",
                (handle, ctx),
            ).fetchone()
            if row is None or row[1] != "trusted":
                for cand in ctx_candidates[1:]:
                    alt = c.execute(
                        "SELECT sk, status FROM incoming WHERE handle = ? AND channel = ?",
                        (handle, cand),
                    ).fetchone()
                    if alt is not None and alt[1] == "trusted":
                        ctx, row = cand, alt
                        break
        if row is None or row[1] != "trusted":
            _dbg(
                f"hook_irc_in_privmsg: no trusted incoming for ({handle},{ctx}) "
                f"row={row} — firing auto-KEYREQ to {nick}"
            )
            last = _rate_limit_sent.get(handle, 0.0)
            now_f = time.time()
            if now_f - last >= KEYREQ_MIN_INTERVAL:
                _rate_limit_sent[handle] = now_f
                # For a DM the wire target is OUR nick — the query buffer is
                # named after the PEER, so search by sender there.
                buf_name = target if readings else nick
                buf = weechat.buffer_search("irc", f"{server}.{buf_name}") if weechat else ""
                try:
                    kreq = build_keyreq(ctx, handle)
                    if weechat:
                        weechat.command(buf, f"/quote NOTICE {nick} :{kreq}")
                        _prnt_ok(buf, f"KEYREQ sent to {nick} for {ctx}")
                        _prnt_dbg(server, ctx, nick, f"TX auto-KEYREQ to {nick} for {ctx}")
                        _dbg(f"hook_irc_in_privmsg: sent auto-KEYREQ to {nick} for {ctx}")
                except Exception as e:
                    if weechat:
                        _prnt_warn(buf, f"automatic KEYREQ to {nick} for {ctx} skipped: {e}")
                    _dbg(f"hook_irc_in_privmsg: build_keyreq raised {e}")
            else:
                _dbg(f"hook_irc_in_privmsg: rate-limited, not sending KEYREQ (last={last} now={now_f})")
            return ""
        sk = row[0]
        aad = build_aad(ctx, wire["msgid"], wire["ts"], wire["part"], wire["total"])
        pt = aead_decrypt(sk, wire["nonce"], aad, wire["ct"])
        if pt is None:
            _dbg(f"hook_irc_in_privmsg: aead_decrypt returned None for ({handle},{ctx})")
            return ""
        _dbg(f"hook_irc_in_privmsg: decrypted {len(pt)} bytes for ({handle},{ctx})")
        try:
            pt_str = pt.decode("utf-8")
        except UnicodeDecodeError:
            pt_str = pt.decode("utf-8", errors="replace")
        # A decrypted non-ACTION CTCP frame (\x01VERSION\x01, \x01PING\x01, …)
        # must NOT be re-injected into weechat's PRIVMSG pipeline: weechat would
        # interpret it and auto-answer with a PLAINTEXT NOTICE. Our own outbound
        # side never encrypts non-ACTION CTCP (it is a cleartext escape hatch),
        # so this is anomalous — drop it. ACTION frames and ordinary text pass
        # through and render normally.
        if pt_str.startswith("\x01") and not pt_str.startswith("\x01ACTION "):
            _dbg(f"hook_irc_in_privmsg: dropping decrypted non-ACTION CTCP from {nick}")
            return ""
        return f":{prefix} PRIVMSG {target} :{pt_str}"
    except Exception as e:
        _dbg(f"hook_irc_in_privmsg OUTER EXCEPTION: {e}\n{traceback.format_exc()}")
        return msg


def hook_input_text_for_buffer(data, modifier, modifier_data, text):
    try:
        if weechat is None:
            return text
        if text.startswith("/"):
            return text
        buffer = modifier_data
        if not buffer:
            return text
        server = weechat.buffer_get_string(buffer, "localvar_server")
        target = weechat.buffer_get_string(buffer, "localvar_channel")
        if not server or not target:
            return text
        plain = text
        # G13: refuse to encrypt an empty line. The user typed either
        # whitespace-only or literally nothing; pass the original text
        # through so weechat can decide what to do with it instead of
        # shipping a zero-ciphertext chunk to the peer.
        if not plain:
            return text
        is_channel = target and target[0] in CHANNEL_PREFIXES
        # Bot-command bypass is CHANNEL-ONLY: `.cmd`/`!cmd` lines go out
        # unencrypted so channel bots can parse them. DMs never bypass —
        # prose starting with '.'/'!' in an E2E DM must still encrypt.
        if is_channel and (plain.startswith(".") or plain.startswith("!")):
            return text
        if is_channel:
            channel = target
        else:
            # Resolve the peer handle live OR from the keyring cache — the same
            # resolution `/e2e on` / the outbound gate use (mirror of Rust
            # `resolve_query_peer_handle`). Live-only resolution would miss the
            # persisted `@<handle>` config whenever the peer isn't currently in
            # a shared channel's nicklist, silently downgrading the DM to
            # plaintext. If neither resolves, the out gate is still the
            # authoritative backstop.
            peer_handle = _resolve_handle_for_command(server, target, target)
            if peer_handle is None:
                _prnt_err(buffer, f"cannot resolve handle for {target} — has the user spoken yet?")
                return text
            channel = "@" + peer_handle
        with db_conn() as c:
            row = c.execute(
                "SELECT enabled FROM channels WHERE channel = ?",
                (channel,),
            ).fetchone()
        if row is None or not row[0]:
            return text
        fresh, had_pending = _get_or_generate_outgoing_key_with_rotation(channel)
        if had_pending:
            _distribute_rekey_safe(channel, fresh, server)
        # Single source of truth for wire framing (shared with the out gate).
        for wire in _encrypt_plain_wire(fresh, channel, plain):
            _send_raw_privmsg(server, target, wire)
        _prnt_self_msg(buffer, plain)
        return ""
    except Exception as e:
        _dbg(f"hook_input_text_for_buffer OUTER EXCEPTION: {e}\n{traceback.format_exc()}")
        return text


def hook_irc_in_notice(data, modifier, server, msg):
    try:
        if not msg.startswith(":"):
            return msg
        prefix_end = msg.index(" ")
        prefix = msg[1:prefix_end]
        rest = msg[prefix_end + 1 :]
        if "!" not in prefix or "@" not in prefix:
            return msg
        nick, userhost = prefix.split("!", 1)
        sender_handle = userhost
        rest_parts = rest.split(" ", 2)
        if len(rest_parts) < 3 or rest_parts[0] != "NOTICE":
            return msg
        text = rest_parts[2][1:] if rest_parts[2].startswith(":") else rest_parts[2]
        if not (text.startswith("\x01") and text.endswith("\x01")) or len(text) < 2:
            return msg
        inner = text[1:-1]
        if not inner.startswith(CTCP_TAG + " "):
            return msg
        _dbg(f"hook_irc_in_notice RPEE2E {inner[:60]!r} from {nick}!{sender_handle}")
        if inner.startswith(CTCP_TAG + " KEYREQ "):
            try:
                parsed = parse_keyreq(inner)
                if parsed is not None:
                    _prnt_dbg(server, parsed["channel"], nick, f"RX KEYREQ from {nick} ({sender_handle}) for {parsed['channel']}")
                rsp_wire, reciprocal = handle_keyreq(server, sender_handle, nick, inner)
            except Exception as e:
                _dbg(f"hook_irc_in_notice KEYREQ EXCEPTION: {e}\n{traceback.format_exc()}")
                return ""
            _dbg(
                f"hook_irc_in_notice KEYREQ processed rsp_wire={'yes' if rsp_wire else 'no'} "
                f"reciprocal={'yes' if reciprocal else 'no'}"
            )
            if rsp_wire is not None and weechat:
                _send_raw_notice(server, nick, rsp_wire)
                if parsed is not None:
                    _prnt_dbg(server, parsed["channel"], nick, f"TX KEYRSP to {nick} for {parsed['channel']}")
                _dbg(f"hook_irc_in_notice sent KEYRSP to {nick} via _send_raw_notice")
            if reciprocal is not None and weechat:
                _send_raw_notice(server, nick, reciprocal)
                if parsed is not None:
                    _prnt_dbg(server, parsed["channel"], nick, f"TX reciprocal KEYREQ to {nick} for {parsed['channel']}")
                _dbg(f"hook_irc_in_notice sent reciprocal KEYREQ to {nick} via _send_raw_notice")
            return ""
        if inner.startswith(CTCP_TAG + " KEYRSP "):
            try:
                result = handle_keyrsp(server, nick, sender_handle, inner)
            except Exception as e:
                _dbg(f"hook_irc_in_notice KEYRSP EXCEPTION: {e}\n{traceback.format_exc()}")
                return ""
            _dbg(f"hook_irc_in_notice KEYRSP processed result={result}")
            return ""
        if inner.startswith(CTCP_TAG + " REKEY "):
            try:
                handle_rekey(sender_handle, nick, inner)
            except Exception as e:
                _dbg(f"hook_irc_in_notice REKEY EXCEPTION: {e}\n{traceback.format_exc()}")
            return ""
        return msg
    except Exception as e:
        _dbg(f"hook_irc_in_notice OUTER EXCEPTION: {e}\n{traceback.format_exc()}")
        return msg


def cmd_e2e(data, buffer, args):
    parts = args.split()
    sub = parts[0].lower() if parts else ""
    rest = parts[1:]
    channel = weechat.buffer_get_string(buffer, "localvar_channel") if weechat else ""
    server = weechat.buffer_get_string(buffer, "localvar_server") if weechat else ""
    buf = buffer if weechat else ""

    if sub in ("", "help"):
        _cmd_help(buf)
    elif sub == "on":
        ctx = _ctx_or_error(buf, buffer, server, channel, None, "/e2e on")
        if ctx is not None:
            with db_conn() as c:
                c.execute(
                    "INSERT OR REPLACE INTO channels VALUES (?, 1, 'normal')",
                    (ctx,),
                )
            _prnt_ok(buf, f"enabled on {ctx} (mode=normal)")
    elif sub == "off":
        ctx = _ctx_or_error(buf, buffer, server, channel, None, "/e2e off")
        if ctx is not None:
            with db_conn() as c:
                c.execute("UPDATE channels SET enabled=0 WHERE channel=?", (ctx,))
            _prnt_ok(buf, f"disabled on {ctx}")
    elif sub == "mode":
        if not rest:
            _prnt_err(buf, "Usage: /e2e mode <auto-accept|normal|quiet>")
        else:
            mode = rest[0].lower()
            if mode not in ("auto-accept", "auto", "normal", "quiet"):
                _prnt_err(buf, f"invalid mode: {mode}")
            else:
                ctx = _ctx_or_error(buf, buffer, server, channel, None, "/e2e mode")
                if ctx is not None:
                    with db_conn() as c:
                        c.execute(
                            "INSERT OR REPLACE INTO channels VALUES (?, 1, ?)",
                            (ctx, mode),
                        )
                    _prnt_ok(buf, f"mode={mode} on {ctx}")
    elif sub == "fingerprint":
        pk, sk, fp = ensure_identity()
        fp_hex = fingerprint_hex(fp)
        sas = fingerprint_bip39(fp)
        if weechat:
            weechat.prnt(buf, f"[E2E] Fingerprint (mine):")
            weechat.prnt(buf, f"  hex  {fp_hex}")
            weechat.prnt(buf, f"  sas  {sas}")
    elif sub == "status":
        with db_conn() as c:
            n = c.execute("SELECT COUNT(*) FROM incoming").fetchone()[0]
            m = c.execute("SELECT COUNT(*) FROM channels WHERE enabled=1").fetchone()[0]
            id_row = c.execute("SELECT fp FROM identity WHERE id=1").fetchone()
        fp_hex = id_row[0].hex() if id_row else "(none)"
        with db_conn() as c:
            ch_row = c.execute(
                "SELECT enabled, mode FROM channels WHERE channel = ?", (channel,)
            ).fetchone()
        chan_info = ""
        if ch_row:
            chan_info = f" [{channel} {'on' if ch_row[0] else 'off'} mode={ch_row[1]} peers={n}]"
        _prnt_ok(buf, f"identity={fp_hex} peers={n} enabled_channels={m}{chan_info}")
    elif sub == "list":
        ctx = _ctx_or_error(buf, buffer, server, channel, None, "/e2e list")
        if ctx is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        with db_conn() as c:
            if ctx.startswith("@"):
                # Query buffer: real DM sessions are recipient-keyed under
                # `@<own>`, trust markers under `@<peer>` — list everything
                # from this peer regardless of context.
                rows = c.execute(
                    "SELECT handle, channel, fp, status FROM incoming WHERE handle = ?",
                    (ctx[1:],),
                ).fetchall()
            else:
                rows = c.execute(
                    "SELECT handle, channel, fp, status FROM incoming WHERE channel = ?",
                    (ctx,),
                ).fetchall()
        if not rows:
            _prnt_ok(buf, "no peers")
        else:
            for r in rows:
                _prnt_ok(buf, f"  {r[0]} on {r[1]}  fp={r[2][:8].hex()}  status={r[3]}")
    elif sub == "accept":
        if not rest:
            _prnt_err(buf, "Usage: /e2e accept <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        ctx = _ctx_or_error(buf, buffer, server, channel, nick, "/e2e accept")
        if ctx is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        handle = _handle_or_error(buf, server, channel, nick, "/e2e accept")
        if handle is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        with db_conn() as c:
            pending = c.execute(
                "SELECT sender_handle, pubkey, eph_x25519, nonce, sig FROM pending_inbound WHERE handle = ? AND channel = ?",
                (handle, ctx),
            ).fetchone()
        if pending is not None:
            s_handle, s_pub, s_eph, s_nonce, s_sig = pending
            with db_conn() as c:
                c.execute(
                    "DELETE FROM pending_inbound WHERE channel = ? AND handle = ?",
                    (ctx, s_handle),
                )
            rsp_wire = _build_keyrsp_for_req(ctx, s_handle, s_pub, s_eph)
            reciprocal = _build_reciprocal_keyreq_on_accept(ctx, s_handle, server)
            if rsp_wire is not None and weechat:
                _send_raw_notice(server, nick, rsp_wire)
            if reciprocal is not None and weechat:
                _send_raw_notice(server, nick, reciprocal)
            _prnt_ok(buf, f"accepted {nick} ({s_handle}) on {ctx} — KEYRSP sent")
        else:
            with db_conn() as c:
                cur = c.execute(
                    "UPDATE incoming SET status='trusted' WHERE handle = ? AND channel = ?",
                    (handle, ctx),
                )
            if cur.rowcount:
                _prnt_ok(buf, f"accepted {nick} ({handle}) on {ctx}")
            else:
                _prnt_err(buf, f"/e2e accept: no pending exchange or session for {nick} on {ctx}")
    elif sub == "decline":
        if not rest:
            _prnt_err(buf, "Usage: /e2e decline <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        ctx = _ctx_or_error(buf, buffer, server, channel, nick, "/e2e decline")
        if ctx is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        handle = _handle_or_error(buf, server, channel, nick, "/e2e decline")
        if handle is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        with db_conn() as c:
            c.execute(
                "DELETE FROM pending_inbound WHERE channel = ? AND handle = ?",
                (ctx, handle),
            )
            c.execute(
                "UPDATE incoming SET status='revoked' WHERE handle = ? AND channel = ?",
                (handle, ctx),
            )
        _prnt_warn(buf, f"declined {nick} on {ctx}")
    elif sub == "revoke":
        if not rest:
            _prnt_err(buf, "Usage: /e2e revoke <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        ctx = _ctx_or_error(buf, buffer, server, channel, nick, "/e2e revoke")
        if ctx is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        handle = _handle_or_error(buf, server, channel, nick, "/e2e revoke")
        if handle is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        _update_incoming_trust(handle, ctx, "revoked")
        with db_conn() as c:
            c.execute(
                "DELETE FROM outgoing_recipients WHERE channel = ? AND handle = ?",
                (ctx, handle),
            )
            c.execute("UPDATE outgoing SET pending_rotation=1 WHERE channel=?", (ctx,))
        _prnt_warn(buf, f"revoked {nick} on {ctx} — key will rotate")
    elif sub == "unrevoke":
        if not rest:
            _prnt_err(buf, "Usage: /e2e unrevoke <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        ctx = _ctx_or_error(buf, buffer, server, channel, nick, "/e2e unrevoke")
        if ctx is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        handle = _handle_or_error(buf, server, channel, nick, "/e2e unrevoke")
        if handle is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        # Mirror of revoke: handle-wide for DMs (see _update_incoming_trust).
        _update_incoming_trust(handle, ctx, "trusted")
        _prnt_ok(buf, f"unrevoked {nick} on {ctx}")
    elif sub == "forget":
        if not rest:
            _prnt_err(buf, "Usage: /e2e forget <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        ctx = _ctx_or_error(buf, buffer, server, channel, nick, "/e2e forget")
        if ctx is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        handle = _handle_or_error(buf, server, channel, nick, "/e2e forget")
        if handle is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        # Handle-wide for DMs (see _update_incoming_trust for why).
        _delete_incoming_rows(handle, ctx)
        _prnt_warn(buf, f"forgotten {nick} on {ctx}")
    elif sub == "handshake":
        if not rest:
            _prnt_err(buf, "Usage: /e2e handshake <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        ctx = _ctx_or_error(buf, buffer, server, channel, nick, "/e2e handshake")
        if ctx is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        # A KEYREQ asks the peer for the key of the direction WE receive, so
        # for a DM it is stamped with OUR handle (recipient-keyed), not the
        # peer's context the config is stored under.
        kreq_ctx = _incoming_ctx_for(server, ctx)
        if kreq_ctx is None:
            _prnt_err(
                buf,
                "/e2e handshake: own identity not learned yet (waiting for "
                "the USERHOST reply) — try again in a moment",
            )
            return weechat.WEECHAT_RC_OK if weechat else 0
        # Best-effort peer handle for the pending key; build_keyreq falls back
        # to a bare-ctx pending row when unresolvable (KEYRSP matches either).
        s_handle = _resolve_handle_for_command(server, channel, nick)
        try:
            kreq = build_keyreq(kreq_ctx, s_handle)
        except Exception as e:
            _prnt_err(buf, f"handshake failed: {e}")
            return weechat.WEECHAT_RC_OK if weechat else 0
        if weechat:
            weechat.command(
                weechat.buffer_search("irc", f"{server}.{channel}"),
                f"/quote NOTICE {nick} :{kreq}",
            )
            _prnt_ok(buf, f"KEYREQ sent to {nick} for {ctx}")
    elif sub == "verify":
        if not rest:
            _prnt_err(buf, "Usage: /e2e verify <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        ctx = _ctx_or_error(buf, buffer, server, channel, nick, "/e2e verify")
        if ctx is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        handle = _handle_or_error(buf, server, channel, nick, "/e2e verify")
        if handle is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        _, _, local_fp = ensure_identity()
        local_sas = fingerprint_bip39(local_fp)
        local_hex = fingerprint_hex(local_fp)
        local_short = local_hex[:16]
        with db_conn() as c:
            # Real DM sessions are recipient-keyed under `@<own>`; fall back
            # to the peer-keyed trust marker if only that exists.
            row = None
            inc_ctx = _incoming_ctx_for(server, ctx)
            for rctx in [c2 for c2 in (inc_ctx, ctx) if c2]:
                row = c.execute(
                    "SELECT fp FROM incoming WHERE handle = ? AND channel = ?",
                    (handle, rctx),
                ).fetchone()
                if row is not None:
                    break
        if row is None:
            _prnt_err(buf, f"no session for {nick} on {ctx}")
        else:
            peer_fp = row[0]
            peer_sas = fingerprint_bip39(peer_fp)
            peer_hex = fingerprint_hex(peer_fp)
            peer_short = peer_hex[:16]
            if weechat:
                weechat.prnt(buf, f"{C_INFO}[E2E] Fingerprint Verification{C_RST}")
                weechat.prnt(buf, f"  You  ( local): {local_short}  {local_sas}")
                weechat.prnt(buf, f"  Them ({nick:<7}): {peer_short}  {peer_sas}")
                weechat.prnt(
                    buf, f"  Read both lines out-of-band and confirm they match."
                )
                weechat.prnt(
                    buf,
                    f"  If they differ, a MitM is in progress — run /e2e forget {nick} immediately.",
                )
    elif sub == "reverify":
        if not rest:
            _prnt_err(buf, "Usage: /e2e reverify <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        handle = _handle_or_error(buf, server, channel, nick, "/e2e reverify")
        if handle is None:
            return weechat.WEECHAT_RC_OK if weechat else 0
        # Resolve nick → canonical handle via the peers table (LIKE prefix
        # match), so notices recorded under e.g. "alice@host" can still be
        # found when the user typed just "alice".
        with db_conn() as c:
            peer = c.execute(
                "SELECT fp, pk, last_handle FROM peers WHERE last_handle LIKE ?",
                (f"{handle}%",),
            ).fetchone()
        canonical_handle = peer[2] if peer and peer[2] else handle
        # Branch 1: drain pending_trust_change for this handle. Look for
        # a FingerprintChanged notice with an attached new pubkey — that's
        # the only combination the automatic apply path can act on without
        # a second handshake. Other match-handle notices are consumed
        # (dropped) so the user doesn't see a duplicate warning after
        # signalling consent via /e2e reverify. Mirrors Rust
        # manager.rs::reverify_peer.
        notices = _take_pending_trust_changes(canonical_handle)
        applied = None  # (new_pubkey, recorded_old_fp, recorded_new_fp)
        for _ch, change, new_pubkey, rec_old_fp, rec_new_fp in notices:
            if (
                applied is None
                and change == "fingerprint_changed"
                and new_pubkey is not None
                and rec_new_fp is not None
            ):
                applied = (new_pubkey, rec_old_fp, rec_new_fp)
        if applied is not None:
            new_pubkey, rec_old_fp, rec_new_fp = applied
            now = int(time.time())
            with db_conn() as c:
                # Delete the old peer row by fingerprint (preferred) or
                # by the looked-up peer row as a fallback.
                if rec_old_fp is not None:
                    c.execute("DELETE FROM peers WHERE fp = ?", (rec_old_fp,))
                elif peer is not None:
                    c.execute("DELETE FROM peers WHERE fp = ?", (peer[0],))
                c.execute(
                    "DELETE FROM incoming WHERE handle LIKE ?", (f"{handle}%",)
                )
                c.execute(
                    "DELETE FROM outgoing_recipients WHERE handle LIKE ?",
                    (f"{handle}%",),
                )
                # Upsert the new identity with Trusted status — the user
                # has consented by running /e2e reverify after comparing
                # the new SAS out-of-band.
                c.execute(
                    "INSERT OR REPLACE INTO peers "
                    "(fp, pk, last_handle, last_nick, first_seen, last_seen, status) "
                    "VALUES (?, ?, ?, ?, ?, ?, 'trusted')",
                    (rec_new_fp, new_pubkey, canonical_handle, None, now, now),
                )
            _prnt_ok(
                buf,
                f"reverified {nick}: accepted new key fp={rec_new_fp.hex()[:16]}",
            )
        elif peer is None:
            _prnt_err(buf, f"no keyring state for {nick} ({handle}) to reverify")
        else:
            # Branch 2: destructive purge fallback — no actionable pending
            # notice found. Remove every trace of this handle so a
            # subsequent handshake starts cold.
            old_fp = peer[0]
            with db_conn() as c:
                c.execute("DELETE FROM peers WHERE fp = ?", (old_fp,))
                c.execute("DELETE FROM incoming WHERE handle LIKE ?", (f"{handle}%",))
                c.execute(
                    "DELETE FROM outgoing_recipients WHERE handle LIKE ?",
                    (f"{handle}%",),
                )
            _prnt_ok(
                buf,
                f"reverified {nick}: purged stale state; re-handshake to TOFU-pin the new key",
            )
    elif sub == "rotate":
        ctx = _ctx_or_error(buf, buffer, server, channel, None, "/e2e rotate")
        if ctx is not None:
            with db_conn() as c:
                c.execute(
                    "UPDATE outgoing SET pending_rotation=1 WHERE channel=?", (ctx,)
                )
            _prnt_ok(buf, f"rotation scheduled for {ctx}")
    elif sub == "export":
        if not rest:
            _prnt_err(buf, "Usage: /e2e export <file>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        path = os.path.expanduser(rest[0])
        try:
            doc = _export_keyring()
            json_str = json.dumps(doc, indent=2)
            with open(path, "w") as f:
                f.write(json_str)
            os.chmod(path, 0o600)
            _prnt_ok(buf, f"exported keyring to {path}")
            _prnt_warn(
                buf, "warning: session keys are in plaintext in this file. Protect it!"
            )
        except Exception as e:
            _prnt_err(buf, f"export failed: {e}")
    elif sub == "import":
        if not rest:
            _prnt_err(buf, "Usage: /e2e import <file>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        path = os.path.expanduser(rest[0])
        try:
            with open(path, "r") as f:
                doc = json.load(f)
            _import_keyring(doc)
            _prnt_ok(buf, f"imported keyring from {path}")
        except Exception as e:
            _prnt_err(buf, f"import failed: {e}")
    elif sub == "autotrust":
        if not rest:
            _cmd_autotrust(buf, [], rest)
        else:
            _cmd_autotrust(buf, rest[0].lower(), rest[1:])
    else:
        _cmd_help(buf)

    return weechat.WEECHAT_RC_OK if weechat else 0


def _cmd_help(buf: str) -> None:
    lines = [
        "[E2E] Encryption commands:",
        "  on                        Enable E2E on the current channel",
        "  off                       Disable E2E on the current channel",
        "  mode <m>                  Set channel mode (auto-accept|normal|quiet)",
        "  fingerprint               Show your fingerprint + SAS words",
        "  list                      List trusted peers",
        "  status                    Show identity + per-channel summary",
        "  accept <nick>             Trust a pending peer",
        "  decline <nick>            Reject a pending peer",
        "  revoke <nick>             Revoke trust; rotate outgoing key",
        "  unrevoke <nick>           Re-trust a previously revoked peer",
        "  forget <nick>             Delete a peer's session",
        "  handshake <nick>          Send KEYREQ to <nick>",
        "  verify <nick>             Show a peer's fingerprint + SAS words",
        "  reverify <nick>           Re-trust after SAS comparison",
        "  rotate                    Schedule outgoing key rotation",
        "  export <file>             Export keyring to JSON",
        "  import <file>             Import keyring from JSON",
        "  autotrust list            List autotrust rules",
        "  autotrust add <scope> <p> Add an autotrust rule",
        "  autotrust remove <p>      Remove an autotrust rule",
        "  help                      Show this index",
    ]
    for line in lines:
        _prnt_ok(buf, line)


def _cmd_autotrust(buf: str, op: str, rest: list[str]) -> None:
    if op == "list" or op == "":
        with db_conn() as c:
            rows = c.execute("SELECT scope, handle_pattern FROM autotrust").fetchall()
        if not rows:
            _prnt_ok(buf, "(no autotrust rules)")
        else:
            for scope, pat in rows:
                _prnt_ok(buf, f"  {scope}  {pat}")
    elif op == "add":
        if len(rest) < 2:
            _prnt_err(buf, "Usage: /e2e autotrust add <scope> <pattern>")
        else:
            scope, pat = rest[0], rest[1]
            with db_conn() as c:
                c.execute(
                    "INSERT OR IGNORE INTO autotrust (scope, handle_pattern, created_at) VALUES (?, ?, ?)",
                    (scope, pat, int(time.time())),
                )
            _prnt_ok(buf, f"autotrust add {scope} {pat}")
    elif op == "remove":
        if not rest:
            _prnt_err(buf, "Usage: /e2e autotrust remove <pattern>")
        else:
            pat = rest[0]
            with db_conn() as c:
                c.execute("DELETE FROM autotrust WHERE handle_pattern = ?", (pat,))
            _prnt_ok(buf, f"autotrust removed {pat}")
    else:
        _prnt_err(buf, "Usage: /e2e autotrust <list|add|remove>")


def _export_keyring() -> dict:
    with db_conn() as c:
        id_row = c.execute(
            "SELECT pk, sk, fp, created_at FROM identity WHERE id = 1"
        ).fetchone()
        if id_row is None:
            raise RuntimeError("no identity present")
        pk, sk, fp, ts = id_row
        peers = c.execute(
            "SELECT fp, pk, last_handle, last_nick, first_seen, last_seen, status FROM peers"
        ).fetchall()
        incoming = c.execute(
            "SELECT handle, channel, fp, sk, status, created_at FROM incoming"
        ).fetchall()
        outgoing = c.execute(
            "SELECT channel, sk, created_at, pending_rotation FROM outgoing"
        ).fetchall()
        channels = c.execute("SELECT channel, enabled, mode FROM channels").fetchall()
        autotrust_rows = c.execute(
            "SELECT scope, handle_pattern, created_at FROM autotrust"
        ).fetchall()
        recipients = c.execute(
            "SELECT channel, handle, fingerprint, first_sent_at FROM outgoing_recipients"
        ).fetchall()

    doc = {
        "version": 1,
        "exportedAt": int(time.time()),
        "identity": {
            "pubkey": pk.hex(),
            "privkey": sk.hex(),
            "fingerprint": fp.hex(),
            "createdAt": ts,
        },
        "peers": [
            {
                "fingerprint": p[0].hex(),
                "pubkey": p[1].hex(),
                "lastHandle": p[2],
                "lastNick": p[3],
                "firstSeen": p[4],
                "lastSeen": p[5],
                "globalStatus": p[6],
            }
            for p in peers
        ],
        "incomingSessions": [
            {
                "handle": s[0],
                "channel": s[1],
                "fingerprint": s[2].hex(),
                "sk": s[3].hex(),
                "status": s[4],
                "createdAt": s[5],
            }
            for s in incoming
        ],
        "outgoingSessions": [
            {
                "channel": o[0],
                "sk": o[1].hex(),
                "createdAt": o[2],
                "pendingRotation": bool(o[3]),
            }
            for o in outgoing
        ],
        "channels": [
            {
                "channel": ch[0],
                "enabled": bool(ch[1]),
                "mode": ch[2],
            }
            for ch in channels
        ],
        "autotrust": [
            {
                "scope": a[0],
                "handlePattern": a[1],
            }
            for a in autotrust_rows
        ],
        "outgoingRecipients": [
            {
                "channel": r[0],
                "handle": r[1],
                "fingerprint": r[2].hex(),
                "firstSentAt": r[3],
            }
            for r in recipients
        ],
    }
    return doc


def _import_keyring(doc: dict) -> None:
    if doc.get("version") != 1:
        raise RuntimeError(f"unsupported export version: {doc.get('version')}")
    identity = doc["identity"]
    pk = bytes.fromhex(identity["pubkey"])
    sk = bytes.fromhex(identity["privkey"])
    fp = bytes.fromhex(identity["fingerprint"])
    ts = identity["createdAt"]
    if len(pk) != 32 or len(sk) != 32 or len(fp) != 16:
        raise RuntimeError("invalid identity field lengths")
    with db_conn() as c:
        c.execute(
            "INSERT OR REPLACE INTO identity VALUES (1, ?, ?, ?, ?)", (pk, sk, fp, ts)
        )
        for p in doc.get("peers", []):
            p_pk = bytes.fromhex(p["pubkey"])
            p_fp = bytes.fromhex(p["fingerprint"])
            c.execute(
                "INSERT OR REPLACE INTO peers (fp, pk, last_handle, last_nick, first_seen, last_seen, status) VALUES (?, ?, ?, ?, ?, ?, ?)",
                (
                    p_fp,
                    p_pk,
                    p.get("lastHandle"),
                    p.get("lastNick"),
                    p.get("firstSeen", 0),
                    p.get("lastSeen", 0),
                    p.get("globalStatus", "pending"),
                ),
            )
        for s in doc.get("incomingSessions", []):
            s_fp = bytes.fromhex(s["fingerprint"])
            s_sk = bytes.fromhex(s["sk"])
            c.execute(
                "INSERT OR REPLACE INTO incoming (handle, channel, fp, sk, status, created_at) VALUES (?, ?, ?, ?, ?, ?)",
                (
                    s["handle"],
                    s["channel"],
                    s_fp,
                    s_sk,
                    s.get("status", "pending"),
                    s.get("createdAt", 0),
                ),
            )
        for o in doc.get("outgoingSessions", []):
            o_sk = bytes.fromhex(o["sk"])
            pr = 1 if o.get("pendingRotation") else 0
            c.execute(
                "INSERT OR REPLACE INTO outgoing VALUES (?, ?, ?, ?)",
                (o["channel"], o_sk, o.get("createdAt", 0), pr),
            )
        for ch in doc.get("channels", []):
            enabled = 1 if ch.get("enabled", False) else 0
            c.execute(
                "INSERT OR REPLACE INTO channels VALUES (?, ?, ?)",
                (ch["channel"], enabled, ch.get("mode", "normal")),
            )
        for a in doc.get("autotrust", []):
            c.execute(
                "INSERT OR IGNORE INTO autotrust (scope, handle_pattern, created_at) VALUES (?, ?, ?)",
                (a["scope"], a["handlePattern"], int(time.time())),
            )
        for r in doc.get("outgoingRecipients", []):
            r_fp = bytes.fromhex(r["fingerprint"])
            c.execute(
                "INSERT OR REPLACE INTO outgoing_recipients (channel, handle, fingerprint, first_sent_at) VALUES (?, ?, ?, ?)",
                (r["channel"], r["handle"], r_fp, r.get("firstSentAt", 0)),
            )


def main() -> None:
    if weechat is None:
        return
    global DB_PATH
    weechat.register(
        SCRIPT_NAME,
        SCRIPT_AUTHOR,
        SCRIPT_VERSION,
        SCRIPT_LICENSE,
        SCRIPT_DESC,
        "",
        "",
    )
    weechat_dir = weechat.info_get("weechat_dir", "") or os.path.expanduser("~/.weechat")
    DB_PATH = os.path.join(weechat_dir, "rpe2e.db")
    init_db()
    ensure_identity()
    weechat.hook_modifier("irc_in2_privmsg", "hook_irc_in_privmsg", "")
    weechat.hook_modifier("input_text_for_buffer", "hook_input_text_for_buffer", "")
    weechat.hook_modifier("irc_in2_notice", "hook_irc_in_notice", "")
    # Authoritative outbound fail-closed gate (F1): every PRIVMSG on the wire —
    # `/me`, `/msg`, `/say`, `/amsg`, plain input — passes through here.
    weechat.hook_modifier("irc_out1_privmsg", "hook_irc_out_privmsg", "")
    # Own-handle tracking for the recipient-keyed DM context
    # (docs/rpe2e-dm-addendum.md): reset+reseed at registration, observe 302
    # (self-USERHOST reply) and our own CHGHOST.
    weechat.hook_signal("irc_server_connected", "hook_signal_server_connected", "")
    weechat.hook_signal("irc_server_disconnected", "hook_signal_server_disconnected", "")
    weechat.hook_modifier("irc_in2_302", "hook_irc_in_302", "")
    weechat.hook_modifier("irc_in2_chghost", "hook_irc_in_chghost", "")
    weechat.hook_modifier("irc_in2_join", "hook_irc_in_join", "")
    weechat.hook_modifier("irc_in2_396", "hook_irc_in_396", "")
    # Script (re)loaded mid-session: `irc_server_connected` will not fire for
    # servers that are already up — seed their own handle now.
    infolist = weechat.infolist_get("irc_server", "", "")
    if infolist:
        while weechat.infolist_next(infolist):
            if weechat.infolist_integer(infolist, "is_connected") == 1:
                _send_self_userhost(weechat.infolist_string(infolist, "name"))
        weechat.infolist_free(infolist)
    weechat.hook_command(
        "e2e",
        SCRIPT_DESC,
        "<on|off|mode|fingerprint|list|status|accept|decline|revoke|unrevoke|forget|handshake|verify|reverify|rotate|export|import|autotrust> [args]",
        "Manage RPE2E end-to-end encryption",
        "on || off || mode auto-accept|normal|quiet || fingerprint || list || status"
        " || accept %(irc_channel_nicks) || decline %(irc_channel_nicks)"
        " || revoke %(irc_channel_nicks) || unrevoke %(irc_channel_nicks)"
        " || forget %(irc_channel_nicks) || rotate"
        " || handshake %(irc_channel_nicks) || verify %(irc_channel_nicks)"
        " || reverify %(irc_channel_nicks)"
        " || export || import"
        " || autotrust list || autotrust add || autotrust remove",
        "cmd_e2e",
        "",
    )
    weechat.prnt(
        "", f"[rpe2e] loaded v{SCRIPT_VERSION}. /e2e fingerprint to view your SAS."
    )


if __name__ == "__main__" or weechat is not None:
    main()

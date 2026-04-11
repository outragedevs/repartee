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
    DB_DIR = weechat.info_get("weechat_dir", "") or os.path.expanduser("~/.weechat")
    DB_PATH = os.path.join(DB_DIR, "rpe2e.db")
else:
    DB_PATH = os.path.expanduser("~/.weechat/rpe2e.db")

_rate_limit_sent: dict[str, float] = {}

_incoming_buckets: dict[str, "IncomingBucket"] = {}


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


def db_conn() -> sqlite3.Connection:
    conn = sqlite3.connect(DB_PATH)
    conn.execute("PRAGMA journal_mode=WAL")
    return conn


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
        c.executescript(SCHEMA_SQL)


def context_key(target: str, handle: str) -> str:
    if target and target[0] in CHANNEL_PREFIXES:
        return target
    return "@" + handle


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


def _ctx_for_target(target: str, handle: str) -> str:
    """Same as context_key but explicit: channel targets pass through,
    PM targets become `@<handle>`. Callers that have a real handle (from
    an inbound message or from a nicklist lookup) pass it here."""
    if target and target[0] in CHANNEL_PREFIXES:
        return target
    return "@" + handle


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


def _prnt_ok(buf: str, msg: str) -> None:
    if weechat:
        weechat.prnt(buf, f"{C_OK}[E2E] {msg}{C_RST}")


def _prnt_err(buf: str, msg: str) -> None:
    if weechat:
        weechat.prnt(buf, f"{C_ERR}[E2E] {msg}{C_RST}")


def _prnt_warn(buf: str, msg: str) -> None:
    if weechat:
        weechat.prnt(buf, f"{C_WARN}[E2E] {msg}{C_RST}")


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
        channel = kv["chan"]
        pub = bytes.fromhex(kv["pub"])
        eph_x25519 = bytes.fromhex(kv["eph"])
        nonce = bytes.fromhex(kv["nonce"])
        sig = bytes.fromhex(kv["sig"])
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
        channel = kv["chan"]
        pub = bytes.fromhex(kv["pub"])
        eph_pub = bytes.fromhex(kv["eph"])
        wnonce = bytes.fromhex(kv["wnonce"])
        wrap_ct = base64.b64decode(kv["wrap"])
        nonce = bytes.fromhex(kv["nonce"])
        sig = bytes.fromhex(kv["sig"])
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
        channel = kv["chan"]
        pub = bytes.fromhex(kv["pub"])
        eph_pub = bytes.fromhex(kv["eph"])
        wnonce = bytes.fromhex(kv["wnonce"])
        wrap_ct = base64.b64decode(kv["wrap"])
        nonce = bytes.fromhex(kv["nonce"])
        sig = bytes.fromhex(kv["sig"])
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


def build_keyreq(channel: str) -> str:
    pk, sk, _fp = ensure_identity()
    eph_sk, eph_pk = generate_x25519_keypair()
    req_nonce = nacl_random(16)
    sig_payload = _sig_payload_keyreq(channel, pk, eph_pk, req_nonce)
    sig = ed25519_sign(sk, sig_payload)
    with db_conn() as c:
        c.execute(
            "INSERT OR REPLACE INTO pending VALUES (?, ?, ?)",
            (channel, eph_sk, int(time.time())),
        )
    body = (
        f"{CTCP_TAG} KEYREQ v=1 chan={channel} pub={pk.hex()} "
        f"eph={eph_pk.hex()} nonce={req_nonce.hex()} sig={sig.hex()}"
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
        f"{CTCP_TAG} KEYRSP v=1 chan={channel} pub={pk.hex()} "
        f"eph={eph_pk.hex()} wnonce={wrap_nonce.hex()} "
        f"wrap={base64.b64encode(wrap_ct).decode()} "
        f"nonce={rsp_nonce.hex()} sig={sig.hex()}"
    )
    return "\x01" + body + "\x01"


def handle_keyreq(sender_handle: str, nick: str, body: str) -> str | None:
    req = parse_keyreq(body)
    if req is None:
        return None
    if not _allow_incoming(sender_handle):
        return None
    sig_payload = _sig_payload_keyreq(
        req["channel"], req["pub"], req["eph_x25519"], req["nonce"]
    )
    if not ed25519_verify(req["pub"], sig_payload, req["sig"]):
        return None
    # The handshake `channel` field is the context key as the sender
    # understood it (channel name or `@<our_handle>` for PMs). We trust
    # that verbatim — the signature binds it.
    ctx = req["channel"]
    with db_conn() as c:
        row = c.execute(
            "SELECT enabled, mode FROM channels WHERE channel = ?", (ctx,)
        ).fetchone()
    if row is None or not row[0]:
        return None
    mode = row[1] if row else "normal"
    peer_fp = fingerprint(req["pub"])
    change = _classify_peer_change(peer_fp, sender_handle)
    if change == "revoked":
        if weechat:
            weechat.prnt(
                "",
                f"{C_WARN}[E2E] WARNING: received KEYREQ from revoked peer {sender_handle}{C_RST}",
            )
        _record_pending_trust_change(
            sender_handle, ctx, "revoked", None, peer_fp, peer_fp
        )
        return None
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
        return None
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
        return None
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
    if effective_mode == "quiet" and not already_trusted:
        return None
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
            buf = weechat.buffer_search("irc", f"*{ctx}")
            weechat.prnt(
                buf,
                f"{C_WARN}[E2E] Pending key exchange from {sender_handle} for {ctx} — /e2e accept <nick>{C_RST}",
            )
        return None
    return _build_keyrsp_for_req(
        req["channel"], sender_handle, req["pub"], req["eph_x25519"]
    )


def handle_keyrsp(sender_handle: str, body: str) -> bool:
    rsp = parse_keyrsp(body)
    if rsp is None:
        return False
    sig_payload = _sig_payload_keyrsp(
        rsp["channel"],
        rsp["pub"],
        rsp["eph_pub"],
        rsp["wrap_nonce"],
        rsp["wrap_ct"],
        rsp["nonce"],
    )
    if not ed25519_verify(rsp["pub"], sig_payload, rsp["sig"]):
        return False
    # `channel` on KEYRSP is the signed context key the initiator used
    # when building the KEYREQ. The responder echoes it verbatim; the
    # signature covers it so we can rely on it as the session key.
    ctx = rsp["channel"]
    with db_conn() as c:
        row = c.execute(
            "SELECT eph_sk FROM pending WHERE channel = ?", (ctx,)
        ).fetchone()
        if row is None:
            return False
        eph_sk = row[0]
        c.execute("DELETE FROM pending WHERE channel = ?", (ctx,))
    shared = x25519_ecdh(eph_sk, rsp["eph_pub"])
    info = b"RPE2E01-WRAP:" + ctx.encode()
    wrap_key = hkdf_sha256(HKDF_SALT, shared, info, KEY_LEN)
    session_key = aead_decrypt(wrap_key, rsp["wrap_nonce"], info, rsp["wrap_ct"])
    if session_key is None or len(session_key) != KEY_LEN:
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
            rsp["pub"],
            old_fp_bytes,
            peer_fp,
        )
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
            return False
        c.execute(
            "INSERT OR REPLACE INTO peers (fp, pk, last_handle, last_nick, first_seen, last_seen, status) VALUES (?, ?, ?, ?, ?, ?, 'trusted')",
            (peer_fp, rsp["pub"], sender_handle, None, now, now),
        )
        c.execute(
            "INSERT OR REPLACE INTO incoming (handle, channel, fp, sk, status, created_at) VALUES (?, ?, ?, ?, 'trusted', ?)",
            (sender_handle, ctx, peer_fp, session_key, now),
        )
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
        f"{CTCP_TAG} REKEY v=1 chan={channel} pub={pk.hex()} "
        f"eph={eph_pk.hex()} wnonce={wrap_nonce.hex()} "
        f"wrap={base64.b64encode(wrap_ct).decode()} "
        f"nonce={nonce.hex()} sig={sig.hex()}"
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
                srv_buf = weechat.buffer_search("irc", f"server.{server}")
                if not srv_buf:
                    srv_buf = weechat.buffer_search("irc", f"{server}.*")
                if srv_buf:
                    weechat.command(srv_buf, f"/quote -server {server} NOTICE {nick} :{ctcp}")
                    continue
            # No server context — send on the currently-active buffer
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
    # G13: refuse empty plaintext outright — mirrors Rust
    # `src/e2e/chunker.rs::split_plaintext`. No zero-length-ciphertext
    # chunk should ever be shipped to a peer.
    if not pt:
        raise ValueError("empty plaintext")
    b = pt.encode("utf-8")
    chunks: list[bytes] = []
    i = 0
    while i < len(b):
        j = min(i + MAX_PT_PER_CHUNK, len(b))
        while j > i and (b[j - 1] & 0xC0) == 0x80:
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

        wire = parse_wire(text)
        if wire is None:
            return msg
        if wire["total"] > MAX_CHUNKS:
            return ""
        skew = abs(int(time.time()) - wire["ts"])
        if skew > TS_TOLERANCE:
            return ""
        ctx = context_key(target, handle)
        with db_conn() as c:
            row = c.execute(
                "SELECT sk, status FROM incoming WHERE handle = ? AND channel = ?",
                (handle, ctx),
            ).fetchone()
        if row is None or row[1] != "trusted":
            last = _rate_limit_sent.get(handle, 0.0)
            now_f = time.time()
            if now_f - last >= KEYREQ_MIN_INTERVAL:
                _rate_limit_sent[handle] = now_f
                try:
                    kreq = build_keyreq(ctx)
                    if weechat:
                        weechat.command(
                            weechat.buffer_search("irc", f"{server}.{target}"),
                            f"/quote NOTICE {nick} :{kreq}",
                        )
                except Exception:
                    pass
            return ""
        sk = row[0]
        aad = build_aad(ctx, wire["msgid"], wire["ts"], wire["part"], wire["total"])
        pt = aead_decrypt(sk, wire["nonce"], aad, wire["ct"])
        if pt is None:
            return ""
        try:
            pt_str = pt.decode("utf-8")
        except UnicodeDecodeError:
            pt_str = pt.decode("utf-8", errors="replace")
        return f":{prefix} PRIVMSG {target} :{pt_str}"
    except Exception:
        return msg


def hook_input_text_display(data, modifier, modifier_data, text):
    try:
        if not text.startswith("PRIVMSG "):
            return text
        _, rest = text.split(" ", 1)
        target, payload = rest.split(" ", 1)
        if not payload.startswith(":"):
            return text
        plain = payload[1:]
        # G13: refuse to encrypt an empty line. The user typed either
        # whitespace-only or literally nothing; pass the original text
        # through so weechat can decide what to do with it instead of
        # shipping a zero-ciphertext chunk to the peer.
        if not plain:
            return text
        ctx = context_key(target, "")
        is_channel = target and target[0] in CHANNEL_PREFIXES
        if is_channel:
            lookup = target
        else:
            lookup = "@" + target
        with db_conn() as c:
            row = c.execute(
                "SELECT enabled FROM channels WHERE channel = ?",
                (lookup if not is_channel else target,),
            ).fetchone()
        if row is None or not row[0]:
            return text
        channel = lookup if not is_channel else target
        fresh, had_pending = _get_or_generate_outgoing_key_with_rotation(channel)
        if had_pending:
            _distribute_rekey(channel, fresh)
        sk = fresh
        chunks = split_plaintext(plain)
        total = len(chunks)
        msgid = nacl_random(8)
        ts = int(time.time())
        server = modifier_data
        wire_lines = []
        for idx, chunk in enumerate(chunks, start=1):
            aad = build_aad(channel, msgid, ts, idx, total)
            nonce, ct = aead_encrypt(sk, aad, chunk)
            wire_lines.append(encode_wire(msgid, ts, idx, total, nonce, ct))
        first = f"PRIVMSG {target} :{wire_lines[0]}"
        for extra in wire_lines[1:]:
            if weechat:
                weechat.command(
                    weechat.buffer_search("irc", f"{server}.{target}"),
                    f"/quote PRIVMSG {target} :{extra}",
                )
        return first
    except Exception:
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
        if inner.startswith(CTCP_TAG + " KEYREQ "):
            rsp_wire = handle_keyreq(sender_handle, nick, inner)
            if rsp_wire is not None and weechat:
                buf = weechat.buffer_search("irc", f"{server}.*")
                weechat.command(buf, f"/quote NOTICE {nick} :{rsp_wire}")
            return ""
        if inner.startswith(CTCP_TAG + " KEYRSP "):
            handle_keyrsp(sender_handle, inner)
            return ""
        if inner.startswith(CTCP_TAG + " REKEY "):
            handle_rekey(sender_handle, nick, inner)
            return ""
        return msg
    except Exception:
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
        if not channel:
            _prnt_err(buf, "/e2e on: no active channel")
        else:
            with db_conn() as c:
                c.execute(
                    "INSERT OR REPLACE INTO channels VALUES (?, 1, 'normal')",
                    (channel,),
                )
            _prnt_ok(buf, f"enabled on {channel} (mode=normal)")
    elif sub == "off":
        if channel:
            with db_conn() as c:
                c.execute("UPDATE channels SET enabled=0 WHERE channel=?", (channel,))
            _prnt_ok(buf, f"disabled on {channel}")
    elif sub == "mode":
        if not rest:
            _prnt_err(buf, "Usage: /e2e mode <auto-accept|normal|quiet>")
        else:
            mode = rest[0].lower()
            if mode not in ("auto-accept", "auto", "normal", "quiet"):
                _prnt_err(buf, f"invalid mode: {mode}")
            else:
                with db_conn() as c:
                    c.execute(
                        "INSERT OR REPLACE INTO channels VALUES (?, 1, ?)",
                        (channel, mode),
                    )
                _prnt_ok(buf, f"mode={mode} on {channel}")
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
        ctx = context_key(channel, "")
        with db_conn() as c:
            rows = c.execute(
                "SELECT handle, channel, fp, status FROM incoming"
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
        ctx = context_key(channel, "")
        with db_conn() as c:
            pending = c.execute(
                "SELECT sender_handle, pubkey, eph_x25519, nonce, sig FROM pending_inbound WHERE handle = ? AND channel = ?",
                (nick if "@" not in nick else nick, ctx),
            ).fetchone()
        if pending is None:
            with db_conn() as c:
                pending = c.execute(
                    "SELECT sender_handle, pubkey, eph_x25519, nonce, sig FROM pending_inbound WHERE channel = ?",
                    (ctx,),
                ).fetchone()
        if pending is not None:
            s_handle, s_pub, s_eph, s_nonce, s_sig = pending
            with db_conn() as c:
                c.execute(
                    "DELETE FROM pending_inbound WHERE channel = ? AND handle = ?",
                    (s_handle, ctx),
                )
            rsp_wire = _build_keyrsp_for_req(channel, s_handle, s_pub, s_eph)
            if rsp_wire is not None and weechat:
                weechat.command(
                    weechat.buffer_search("irc", f"{server}.*"),
                    f"/quote NOTICE {nick} :{rsp_wire}",
                )
            _prnt_ok(buf, f"accepted {nick} ({s_handle}) on {ctx} — KEYRSP sent")
        else:
            with db_conn() as c:
                c.execute(
                    "UPDATE incoming SET status='trusted' WHERE handle LIKE ? AND channel = ?",
                    (f"{nick}%", ctx),
                )
            _prnt_ok(buf, f"accepted {nick} on {ctx}")
    elif sub == "decline":
        if not rest:
            _prnt_err(buf, "Usage: /e2e decline <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        ctx = context_key(channel, "")
        with db_conn() as c:
            c.execute(
                "DELETE FROM pending_inbound WHERE channel = ? AND handle LIKE ?",
                (ctx, f"{nick}%"),
            )
            c.execute(
                "UPDATE incoming SET status='revoked' WHERE handle LIKE ? AND channel = ?",
                (f"{nick}%", ctx),
            )
        _prnt_warn(buf, f"declined {nick} on {ctx}")
    elif sub == "revoke":
        if not rest:
            _prnt_err(buf, "Usage: /e2e revoke <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        ctx = context_key(channel, "")
        handle = nick
        with db_conn() as c:
            c.execute(
                "UPDATE incoming SET status='revoked' WHERE handle LIKE ? AND channel = ?",
                (f"{handle}%", ctx),
            )
            c.execute(
                "DELETE FROM outgoing_recipients WHERE channel = ? AND handle LIKE ?",
                (ctx, f"{handle}%"),
            )
            c.execute("UPDATE outgoing SET pending_rotation=1 WHERE channel=?", (ctx,))
        _prnt_warn(buf, f"revoked {nick} on {ctx} — key will rotate")
    elif sub == "unrevoke":
        if not rest:
            _prnt_err(buf, "Usage: /e2e unrevoke <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        ctx = context_key(channel, "")
        with db_conn() as c:
            c.execute(
                "UPDATE incoming SET status='trusted' WHERE handle LIKE ? AND channel = ?",
                (f"{nick}%", ctx),
            )
        _prnt_ok(buf, f"unrevoked {nick} on {ctx}")
    elif sub == "forget":
        if not rest:
            _prnt_err(buf, "Usage: /e2e forget <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        ctx = context_key(channel, "")
        with db_conn() as c:
            c.execute(
                "DELETE FROM incoming WHERE handle LIKE ? AND channel = ?",
                (f"{nick}%", ctx),
            )
        _prnt_warn(buf, f"forgotten {nick} on {ctx}")
    elif sub == "handshake":
        if not rest:
            _prnt_err(buf, "Usage: /e2e handshake <nick>")
            return weechat.WEECHAT_RC_OK if weechat else 0
        nick = rest[0]
        if not channel:
            _prnt_err(buf, "/e2e handshake: no active channel")
            return weechat.WEECHAT_RC_OK if weechat else 0
        ctx = context_key(channel, "")
        try:
            kreq = build_keyreq(ctx)
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
        ctx = context_key(channel, "")
        _, _, local_fp = ensure_identity()
        local_sas = fingerprint_bip39(local_fp)
        local_hex = fingerprint_hex(local_fp)
        local_short = local_hex[:16]
        with db_conn() as c:
            row = c.execute(
                "SELECT fp FROM incoming WHERE handle LIKE ? AND channel = ?",
                (f"{nick}%", ctx),
            ).fetchone()
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
        handle = nick
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
        if not channel:
            _prnt_err(buf, "/e2e rotate: no active channel")
        else:
            with db_conn() as c:
                c.execute(
                    "UPDATE outgoing SET pending_rotation=1 WHERE channel=?", (channel,)
                )
            _prnt_ok(buf, f"rotation scheduled for {channel}")
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
    weechat.register(
        SCRIPT_NAME,
        SCRIPT_AUTHOR,
        SCRIPT_VERSION,
        SCRIPT_LICENSE,
        SCRIPT_DESC,
        "",
        "",
    )
    init_db()
    ensure_identity()
    weechat.hook_modifier("irc_in_privmsg", "hook_irc_in_privmsg", "")
    weechat.hook_modifier("irc_out_privmsg", "hook_input_text_display", "")
    weechat.hook_modifier("irc_in_notice", "hook_irc_in_notice", "")
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

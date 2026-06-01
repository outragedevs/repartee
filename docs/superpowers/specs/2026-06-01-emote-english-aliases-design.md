# Emote English Aliases + Picker Language — Design

- **Date:** 2026-06-01
- **Status:** Proposed (awaiting approval)
- **Topic:** Give every built-in GG7 emote an English alias so `:name:` accepts
  both the Polish original and an English name, and add an `emotes.lang` setting
  (default `en`) that controls the picker/autocomplete preview language.

## 1. Goal

The 183 emotes are named in Polish (file stems, e.g. `usmiech`, `je_pizze`).
Add a thoughtful English alias for each so users can type either `:smile:` or
`:usmiech:` and get the same emote. A new `emotes.lang` setting (default English)
controls which name the picker shows and which name the picker/Tab inserts.

## 2. Locked decisions (from clarifying questions)

- **`:tag:` accepts BOTH** the Polish stem and the English alias; both render the
  same GIF (TUI and web).
- **Picker / Tab insertion follows `emotes.lang`**: with `en` it inserts the
  English name (`:smile:`), with `pl` the Polish (`:usmiech:`). The other name
  still works when typed by hand.
- **Autocomplete always offers BOTH** languages (`:smi`→`:smile:`, `:usm`→
  `:usmiech:`) regardless of `emotes.lang` — both are valid tags. `emotes.lang`
  only affects picker labels and which name a pick/Tab inserts.
- **`emotes.lang = en | pl`, default `en`**, runtime-settable via `/set`.

## 3. Architecture / data model

**Single source of truth:** `assets/emotes/aliases.tsv` — one
`polish_stem<TAB>english_alias` line per emote (183 lines, validated: full
coverage, unique English aliases, no English alias collides with a different
Polish stem, all `[a-z0-9_]`). A handful of loanword emotes map to themselves
(`lol→lol`, `ok→ok`, `menu→menu`, `wow→wow`, `peace→peace`, `stop→stop`,
`sex→sex`, `killer→killer`, `8p→8p`) — i.e. no distinct alias.

**Native crate (`src/emotes/`):** `include_str!("../../assets/emotes/aliases.tsv")`
parsed in a `LazyLock`. No `build.rs` needed. The `emote_index` (and the PUA
placeholder encoding) still index the **Polish stems** = file names, so the
animator/renderer are unchanged.

New registry API (additive):
- `names() -> &[String]` — unchanged: sorted Polish stems (canonical, = files).
- `english_label(index) -> Option<&'static str>` — English alias for the picker
  (`None`/equal-to-stem when no distinct alias).
- `resolve(name) -> Option<u32>` — map a typed name (Polish **or** English) to
  its canonical index. `contains(name)` becomes `resolve(name).is_some()`.
- `bytes(name)` resolves English→stem→file. The axum route `/emotes/{file}`
  stays **Polish-only**; the web frontend resolves English→stem before building
  `src="/emotes/<stem>.gif"`.
- `all_tag_names() -> &[&str]` — union of stems + English aliases, for the
  tokenizer whitelist and autocomplete.

**Web crate (`web-ui/`):** `build.rs` already reads `../assets/emotes/`; extend
it to also parse `aliases.tsv` and generate: (a) the whitelist set (stems + EN
aliases) for `emotify_spans`, and (b) an `english→stem` map so the `<img>` `src`
uses the Polish stem the server serves. `web-ui/src/emotes.rs` exposes
`is_emote(name)` (either language) and `stem_for(name) -> &str`.

**Tokenizers** (native `parse.rs` + web `format.rs`): the whitelist predicate
now accepts both languages; matching rule (`:` + `[a-z0-9_]+` + `:`) unchanged.

## 4. Configuration

```toml
[emotes]
enabled = true
render  = "graphical"
lang    = "en"          # en | pl — picker/autocomplete-insert preview language
```

`emotes.lang` is runtime-settable via `/set emotes.lang en|pl` (added to the
`emotes` section get/set + settable paths). It affects **only** the TUI picker
labels and which name the picker/Tab inserts. It is **not** pushed to the web UI
(the web has no picker; the web tokenizer already accepts both names via the
build-time whitelist).

## 5. Behavior

- **Tokenizer / rendering:** `:smile:` and `:usmiech:` both render the emote, in
  TUI and web.
- **Picker:** labels shown in `emotes.lang` (English by default); filtering
  matches either language (case-insensitive); Enter/click inserts the
  `emotes.lang` name.
- **Tab-complete:** offers both languages; the completion inserts the matched
  name as typed (so `:smi`→`:smile:`, `:usm`→`:usmiech:`).
- **`/emote <name>`:** accepts either language (case-insensitive), inserts the
  canonical name for the current `emotes.lang`.

## 6. Testing

- `aliases.tsv` integrity test: 183 rows, every stem is a known emote, English
  aliases unique, no English alias equals a different stem, charset `[a-z0-9_]`.
- `resolve()`: Polish stem, English alias, and unknown all map correctly;
  `:smile:` and `:usmiech:` resolve to the same index.
- tokenizer: both names tokenize; `:)`/unknown unaffected.
- web `is_emote`/`stem_for`: English resolves to the right stem; `<img src>` uses
  the stem.
- picker `filtered_indices`: matches by either language.

## 7. Translation table (183)

`got_smacked` (krecka_dostal) and `lousy` (dobani) are best-guesses — correct if
needed. `8p` keeps the same name in both languages.

```
3m_sie	take_care
8p	8p
aniolek	angel
aparat	camera
beczy	bawling
beksa	crybaby
bezradny	helpless
bije	hitting
boisie	scared
boje_sie	afraid
boks	boxing
brawa	applause
buja_w_oblokach	daydreaming
bukiet	bouquet
buziak	kiss
buzki	kisses
calus	smooch
cfaniak	wise_guy
chatownik	chatter
chytry	cunning
cisza	silence
cmok	mwah
co	what
co_jest	whats_up
cwaniak	sly_guy
czarodziej	wizard
czas	time
czytaj	read
diabelek	devil
dobani	lousy
dokuczacz	teaser
dostal	got_hit
dresiarz	chav
drink	cocktail
dupa	butt
faja	cigarette
figielek	mischief
foch	sulk
fuck	middle_finger
gafa	blunder
ganja	weed
gazeta	newspaper
glaszcze	petting
glupek	fool
glupek2	fool2
gool	goal
gra	gaming
haha	laughing
hahaha	lmao
heej	heey
hejka	hiya
hmmm	hmm
hura	hooray
jablko	apple
jem	eating
je_pizze	eating_pizza
jezyk	tongue
jezyk1	tongue1
jezyk2	tongue2
jezyk_oko	tongue_wink
jupi	yippee
kawa	coffee
killer	killer
klotnia	argument
kotek	kitten
krecka_dostal	got_smacked
krzyk	scream
krzywy	wonky
kwadr	square_smile
kwasny	sour
kwiatek	flower
list	letter
lol	lol
luzik	chill
menu	menu
milczek	quiet_one
milosc	love
mniam	yum
mruga	blinking
mutny	sad_mood
muza	music
mysli	thoughts
nauka	studying
nerwus	nervous
nie	no
niee	nooo
nie_powiem	wont_tell
nonono	tsk_tsk
obiad	lunch
oczko	wink
oczko2	wink2
oczy	eyes
ok	ok
ok2	ok2
oklasky	clapping
okok	okay_okay
okularnik	glasses_guy
olaboga	omg
onajego	cheek_kiss
ostr	smirk
pada	raining
paker	buff
palacz	smoker
paluszkiem	wagging_finger
papa	bye
papa2	bye2
peace	peace
pies	dog
pisze	typing
piwko2	beer2
piwo	beer
placze	crying
plask	slap
plotki	gossip
pocieszacz	consoler
pomocy	help
prezent	gift
prosi	begging
prysznic	shower
przytul	hug
puknijsie	are_you_nuts
pytajnik	question_mark
rotfl	rotfl
roza	rose
sciana	wall
serce	heart
serducho	big_heart
serduszka	hearts
serduszka2	hearts2
sex	sex
slonko	sun
smutny	sad
snieg	snow
soczek	juice
spadaj	get_lost
spie	sleeping
spioch	sleepyhead
spoko	no_worries
stop	stop
stres	stress
szampan	champagne
szok	shock
tak	yes
tancze	dancing
telefon	phone
tiaaa	yeah_right
tort	cake
tuptup	stomping
uczen	student
uoeee	waaah
uscisk	embrace
usmiech	smile
usmiech2	smile2
usta	lips
w8	wait
wanna	bathtub
wc	toilet
wesoly	cheerful
winko	wine
wnerw	annoyed
wow	wow
wsciekly	furious
wstydnis	shy
wykrzyknik	exclamation
wysmiewacz	mocker
ysz	wtf
yyyy	uhh
zab	tooth
zacieszacz	delight
zakochany	in_love
zakupy	shopping
zalamka	breakdown
zawstydzony	embarrassed
zdziwko	surprised
zeby	teeth
zegar	clock
ziew	yawn
z_jezorem	big_tongue
zlezkawoku	teary
zly	angry
zmeczony	tired
zniesmaczony	disgusted
zygi	puking
```

## 8. Out of scope

- Localizing emote names beyond English/Polish.
- Pushing `emotes.lang` to the web UI (no web picker).
- Renaming the GIF files (Polish stems remain the canonical file names).

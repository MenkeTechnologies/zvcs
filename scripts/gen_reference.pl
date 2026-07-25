#!/usr/bin/env perl
# Generate docs/reference.html from the man-page source of truth.
#
# `src/extensions/src/superset/manpage.rs` holds one `Doc` per superset (z*)
# verb — the same table `git help <verb>` renders to roff. This script parses
# that table and emits the static reference page, so the two can never drift
# and no count, version, or verb name is typed by hand.
#
#   perl scripts/gen_reference.pl            # writes docs/reference.html
#   perl scripts/gen_reference.pl --check    # exit 1 if the file is stale

use strict;
use warnings;
use File::Basename qw(dirname);

my $root     = dirname(dirname(__FILE__));
my $src      = "$root/src/extensions/src/superset/manpage.rs";
my $cargo    = "$root/src/extensions/Cargo.toml";
my $out      = "$root/docs/reference.html";
my $check    = grep { $_ eq '--check' } @ARGV;

# ---------------------------------------------------------------- parse input

sub slurp {
    my ($path) = @_;
    open my $fh, '<:encoding(UTF-8)', $path or die "gen_reference: $path: $!\n";
    local $/;
    <$fh>;
}

my $rs = slurp($src);
my ($docs) = $rs =~ /^pub const DOCS: &\[Doc\] = &\[\n(.*?)^\];/ms
    or die "gen_reference: no DOCS table in $src\n";

my ($version) = slurp($cargo) =~ /^version\s*=\s*"([^"]+)"/m
    or die "gen_reference: no version in $cargo\n";

# Rust string literal, honouring backslash escapes.
my $STR = qr/"((?:[^"\\]|\\.)*)"/;

# `\(em` and friends are roff glyph escapes; the page is HTML. `\t` is left as
# written — a literal tab would collapse to a space in inline <code>, and the
# only use is spelling out a TSV layout, where the escape is what reads right.
my %ROFF = (
    '\(em'   => "\x{2014}",    # em dash
    '\(en'   => "\x{2013}",    # en dash
    '\(<->'  => "\x{2194}",    # left-right arrow
);

sub unescape {
    my ($s) = @_;
    $s =~ s/\\(["\\])/$1/g;                       # Rust escapes
    $s =~ s/(\\\(em|\\\(en|\\\(<->)/$ROFF{$1}/g;  # roff escapes
    $s;
}

sub html {
    my ($s) = @_;
    $s =~ s/&/&amp;/g;
    $s =~ s/</&lt;/g;
    $s =~ s/>/&gt;/g;
    $s =~ s/`([^`]+)`/<code>$1<\/code>/g;         # markdown-ish inline code
    $s;
}

sub field { my ($blob, $name) = @_; unescape(($blob =~ /$name: $STR/)[0] // '') }

my @verbs;
for my $blob (split /^    Doc \{$/m, $docs) {
    next unless $blob =~ /verb: "/;
    my ($desc_blob) = $blob =~ /desc: &\[(.*?)\],?\s*\}/s;
    my @desc;
    while (($desc_blob // '') =~ /$STR/g) { push @desc, unescape($1) }
    push @verbs, {
        verb     => field($blob, 'verb'),
        summary  => field($blob, 'summary'),
        synopsis => field($blob, 'synopsis'),
        desc     => \@desc,
    };
}

@verbs or die "gen_reference: parsed zero verbs from $src\n";
for my $v (@verbs) {
    @{ $v->{desc} } or die "gen_reference: $v->{verb} has no description\n";
    $v->{synopsis} or die "gen_reference: $v->{verb} has no synopsis\n";
}

# ------------------------------------------------------------------ emit page

my $count = scalar @verbs;
my $repo  = 'https://github.com/MenkeTechnologies/zvcs';

my $index = join "\n", map {
    sprintf '        <li><a href="#%s">%s</a> <span class="verb-summary">%s</span></li>',
        $_->{verb}, html($_->{verb}), html($_->{summary})
} @verbs;

my $entries = join "\n", map {
    my $v = $_;
    join "\n",
        qq{      <article class="doc-entry" id="$v->{verb}">},
        qq{        <h3><span class="doc-sigil">git</span> } . html($v->{verb})
            . qq{ <span class="doc-summary">} . html($v->{summary}) . qq{</span></h3>},
        qq{        <pre class="doc-synopsis"><code>} . html($v->{synopsis}) . qq{</code></pre>},
        (map { '        <p>' . html($_) . '</p>' } @{ $v->{desc} }),
        qq{        <a class="doc-top" href="#verb-index">\x{2191} index</a>},
        qq{      </article>};
} @verbs;

my $page = <<"HTML";
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="dark light">
  <meta name="description" content="zvcs superset verb reference \x{2014} every z* verb with its synopsis and manual text, generated from the same table git help &lt;verb&gt; renders.">
  <title>zvcs \x{2014} Verb Reference</title>
  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link href="https://fonts.googleapis.com/css2?family=Orbitron:wght\@400;600;700;900&amp;family=Share+Tech+Mono&amp;display=swap" rel="stylesheet">
  <link rel="stylesheet" href="hud-static.css">
  <link rel="stylesheet" href="tutorial.css">
  <style>
    .tutorial-main { max-width: 72rem; }
    .docs-build-line { margin: 0.35rem 0 0; font-family: 'Share Tech Mono', ui-monospace, monospace; font-size: 11px; color: var(--text-dim); letter-spacing: 0.03em; max-width: 48rem; opacity: 0.75; }
    .hub-scheme-strip { border-bottom: 1px dashed var(--border); background: color-mix(in srgb, var(--bg-secondary) 85%, transparent); padding: 0.55rem 1.5rem 0.65rem; position: relative; }
    .hub-scheme-strip-inner { max-width: 72rem; margin: 0 auto; display: flex; align-items: center; gap: 0.85rem; }
    .hub-scheme-strip .hud-scheme-label { flex: 0 0 auto; font-family: 'Orbitron', sans-serif; font-size: 9px; font-weight: 700; letter-spacing: 2px; text-transform: uppercase; color: var(--accent); }
    .hub-scheme-strip .scheme-grid { flex: 1 1 auto; display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); gap: 6px; }
    \@media (max-width: 720px) { .hub-scheme-strip-inner { flex-direction: column; align-items: stretch; } .hub-scheme-strip .scheme-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); } }

    .verb-index { list-style: none; padding: 0; margin: 0; display: grid; grid-template-columns: repeat(auto-fill, minmax(21rem, 1fr)); gap: 0.3rem; }
    .verb-index li { border: 1px solid var(--border); border-left: 2px solid var(--cyan); padding: 0.4rem 0.65rem; border-radius: 2px; background: color-mix(in srgb, var(--bg-card) 92%, transparent); }
    .verb-index a { font-family: 'Share Tech Mono', ui-monospace, monospace; font-size: 13px; color: var(--cyan); text-decoration: none; }
    .verb-index a:hover { color: var(--accent-light); }
    .verb-summary { display: block; font-size: 11px; color: var(--text-muted); line-height: 1.45; margin-top: 0.15rem; }

    .doc-entry { border: 1px solid var(--border); border-left: 2px solid var(--magenta); border-radius: 2px; background: color-mix(in srgb, var(--bg-card) 92%, transparent); padding: 0.85rem 1.1rem; margin: 0 0 0.6rem; scroll-margin-top: 1rem; }
    .doc-entry h3 { margin: 0 0 0.5rem; font-family: 'Orbitron', sans-serif; font-size: 13px; font-weight: 700; letter-spacing: 1.2px; color: var(--magenta); }
    .doc-sigil { color: var(--text-muted); font-weight: 400; }
    .doc-summary { display: block; margin-top: 0.25rem; font-family: 'Share Tech Mono', ui-monospace, monospace; font-size: 11.5px; font-weight: 400; letter-spacing: 0; text-transform: none; color: var(--text-dim); }
    .doc-synopsis { font-family: 'Share Tech Mono', ui-monospace, monospace; font-size: 12px; background: var(--bg); border: 1px solid var(--border); border-radius: 2px; padding: 0.55rem 0.8rem; overflow-x: auto; margin: 0 0 0.6rem; box-shadow: inset 0 0 18px rgba(0, 0, 0, 0.35); }
    .doc-synopsis code { color: var(--cyan); background: transparent; border: none; padding: 0; white-space: pre; }
    [data-theme="light"] .doc-synopsis { box-shadow: inset 0 0 10px rgba(0, 0, 0, 0.05); }
    .doc-entry p { margin: 0 0 0.45rem; font-size: 12.5px; color: var(--text-dim); line-height: 1.65; }
    .doc-entry p code { font-family: 'Share Tech Mono', ui-monospace, monospace; font-size: 11.5px; color: var(--accent-light); }
    .doc-top { font-family: 'Share Tech Mono', ui-monospace, monospace; font-size: 10px; color: var(--text-muted); text-decoration: none; }
    .doc-top:hover { color: var(--cyan); }
  </style>
</head>
<body>
  <div class="app tutorial-app" id="docsApp">
    <div class="crt-scanline" id="crtH" aria-hidden="true"></div>
    <div class="crt-scanline-v" id="crtV" aria-hidden="true"></div>

    <header class="tutorial-header">
      <div class="tutorial-header-inner">
        <div>
          <h1 class="tutorial-brand">// ZVCS \x{2014} VERB REFERENCE</h1>
          <nav class="tutorial-crumbs" aria-label="Breadcrumb">
            <a href="index.html">Docs</a>
            <span class="sep">/</span>
            <span class="current">Verb Reference</span>
            <span class="sep">/</span>
            <a href="report.html">Engineering Report</a>
            <span class="sep">/</span>
            <a href="$repo" target="_blank" rel="noopener noreferrer">GitHub</a>
          </nav>
          <p class="docs-build-line">zvcs v$version \x{b7} $count superset verbs \x{b7} generated from <code>src/extensions/src/superset/manpage.rs</code>, the same table <code>git help &lt;verb&gt;</code> renders</p>
        </div>
        <div class="tutorial-toolbar">
          <button type="button" class="btn btn-secondary" id="btnTheme" title="Toggle light/dark">Theme</button>
          <button type="button" class="btn btn-secondary active" id="btnCrt" title="CRT scanline overlay">CRT</button>
          <button type="button" class="btn btn-secondary active" id="btnNeon" title="Neon border pulse">Neon</button>
          <a class="btn btn-secondary" href="index.html">Hub</a>
          <a class="btn btn-secondary" href="report.html">Report</a>
          <a class="btn btn-secondary" href="$repo" target="_blank" rel="noopener noreferrer">GitHub</a>
        </div>
      </div>
    </header>

    <div class="hub-scheme-strip">
      <div class="hub-scheme-strip-inner">
        <span class="hud-scheme-label">// Color scheme</span>
        <div class="scheme-grid" id="hudSchemeGrid"></div>
      </div>
    </div>

    <main class="tutorial-main">
      <h2 class="tutorial-title"><span class="step-hash">&gt;_</span>SUPERSET VERBS</h2>
      <p class="tutorial-subtitle">The $count <code>z*</code> verbs \x{2014} the coordination layer stock git has no equivalent for. Every entry is the verb's man page: <code>git help &lt;verb&gt;</code> shows the same text in a terminal.</p>

      <section class="tutorial-section" id="verb-index">
        <h2>Index</h2>
        <ul class="verb-index">
$index
        </ul>
      </section>

      <section class="tutorial-section">
        <h2>Manual</h2>
$entries
      </section>
    </main>
  </div>
  <script src="hud-theme.js"></script>
</body>
</html>
HTML

if ($check) {
    my $have = -e $out ? slurp($out) : '';
    if ($have ne $page) {
        die "gen_reference: docs/reference.html is stale \x{2014} run perl scripts/gen_reference.pl\n";
    }
    print "gen_reference: docs/reference.html up to date ($count verbs)\n";
    exit 0;
}

open my $fh, '>:encoding(UTF-8)', $out or die "gen_reference: $out: $!\n";
print $fh $page;
close $fh;
print "gen_reference: wrote docs/reference.html ($count verbs, v$version)\n";

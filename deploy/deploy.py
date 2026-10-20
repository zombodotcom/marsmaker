"""Build the GitHub Pages site and publish it to the gh-pages branch.

    python deploy/deploy.py [--pkg D:/marsmaker_data/web/pkg] [--out D:/marsmaker_data/pages_build]
                            [--no-pack] [--no-push] [--remote https://github.com/zombodotcom/marsmaker.git]
                            [--netlify [--netlify-site marsmaker]]

Steps:
1. deploy/pack_sites.py: pack the sites in deploy/sites.txt (skips ones already packed), make
   thumbs/ and sites.json.
2. Copy deploy/index.html, the wasm build (pkg/, from viewer/WEB.md) and play.html into OUT.
   play.html is viewer/web/index.html with a small overlay injected (back link, help hint and
   on-screen touch buttons that send key events to the canvas); the original file is not changed.
   Live mode: viewer/web/live.html (site picker, its play-page meta pointed at play.html) and
   viewer/web/live_index.json go next to play.html; play.html?live=<id> streams from NASA.
3. Publish OUT as ONE orphan commit force-pushed to gh-pages, from a separate git dir
   (OUT/_publish.git, work tree = OUT). Only the files listed in sites.json are committed, so the
   branch never accumulates history and stale sites on disk are not deployed.
4. Make sure GitHub Pages serves the gh-pages branch (gh api repos/<owner>/<repo>/pages).

5. With --netlify, also publish the same files to Netlify (https://<site>.netlify.app): they are
   hard-linked into OUT/_netlify with deploy/netlify.toml (headers, caching) and sent with
   `netlify deploy --prod`. The site is created on first use. --no-push --netlify skips GitHub.

Limits checked before pushing: every file < 100 MB, total < 950 MB.
"""
import argparse, json, os, shutil, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)

OVERLAY = r"""
  <style>
    #mm-bar { position: absolute; top: 10px; right: 10px; display: flex; gap: 8px; z-index: 5;
      font: 14px system-ui, sans-serif; }
    #mm-bar a, #mm-bar button { background: #1a0d08b0; color: #fff3e6; border: 1px solid #ffffff33; border-radius: 999px;
      padding: 6px 12px; text-decoration: none; cursor: pointer; font: inherit; }
    #mm-help { position: absolute; top: 50px; right: 10px; max-width: 320px; z-index: 5; background: #1a0d08d0;
      color: #fff3e6; border-radius: 10px; padding: 10px 14px; font: 13px/1.5 system-ui, sans-serif; transition: opacity 1s; }
    #mm-help kbd { background: #3a1f12; border-radius: 4px; padding: 0 5px; font: 600 12px ui-monospace, monospace; }
    #mm-touch { position: absolute; inset: auto 0 0 0; display: none; justify-content: space-between; padding: 0 14px 18px;
      pointer-events: none; z-index: 5; }
    #mm-touch .grp { display: flex; gap: 12px; pointer-events: auto; }
    #mm-touch .grp.v { flex-direction: column; }
    #mm-touch button { width: 72px; height: 72px; border-radius: 50%; border: 2px solid #ffffff55; background: #1a0d0870;
      color: #fff; font: 700 24px system-ui, sans-serif; touch-action: none; user-select: none; -webkit-user-select: none; }
    #mm-touch button.on { background: #e8743bb0; }
    #mm-touch .small { width: 52px; height: 52px; font-size: 14px; }
    #sites { display: none; }  /* the viewer's own live-sites link; the bar below has one */
    @media (pointer: coarse) { #mm-touch { display: flex; } }
    @media (max-width: 420px) { #mm-touch { padding: 0 8px 14px; } #mm-touch .grp { gap: 8px; }
      #mm-touch button { width: 60px; height: 60px; } #mm-touch .small { width: 46px; height: 46px; font-size: 13px; } }
  </style>
  <div id="mm-bar"><a href="./">&larr; Sites</a><a href="live.html">All 1,262</a><button id="mm-q" type="button">?</button></div>
  <div id="mm-help"><kbd>W</kbd>/<kbd>&uarr;</kbd> throttle &nbsp;<kbd>S</kbd>/<kbd>&darr;</kbd> reverse &nbsp;
    <kbd>A</kbd><kbd>D</kbd> steer<br><kbd>Space</kbd> brake &nbsp;<kbd>R</kbd> flip upright &nbsp;<kbd>C</kbd> camera<br>
    <kbd>Shift</kbd> boost &nbsp;<kbd>Ctrl</kbd>/<kbd>J</kbd> jump &nbsp;in the air <kbd>W</kbd><kbd>S</kbd> pitch, <kbd>A</kbd><kbd>D</kbd> roll
    (FUN mode) &nbsp;<kbd>M</kbd> FUN / REAL mode<br>
    Click the terrain first so it gets the keyboard. The page may freeze a few seconds while the site decodes.</div>
  <div id="mm-touch">
    <div class="grp"><button data-code="ArrowLeft" aria-label="steer left">&#9664;</button><button data-code="ArrowRight" aria-label="steer right">&#9654;</button>
      <div class="grp v"><button class="small" data-code="KeyJ" aria-label="jump">jump</button><button class="small" data-code="ShiftLeft" aria-label="boost">boost</button></div></div>
    <div class="grp"><div class="grp v"><button class="small" data-code="KeyR" aria-label="flip upright">flip</button><button class="small" data-code="KeyC" aria-label="camera">cam</button></div>
      <div class="grp v"><button data-code="ArrowUp" aria-label="throttle">&#9650;</button><button data-code="ArrowDown" aria-label="reverse">&#9660;</button></div></div>
  </div>
  <script>
    (function () {
      // MarsMaker Pages overlay (injected by deploy/deploy.py)
      const canvas = document.getElementById('bevy');
      const help = document.getElementById('mm-help');
      let hideT = setTimeout(() => { help.style.opacity = 0; }, 12000);
      document.getElementById('mm-q').onclick = () => {
        clearTimeout(hideT); help.style.opacity = help.style.opacity === '0' ? 1 : 0; canvas.focus();
      };
      const keyOf = { ArrowLeft: 'ArrowLeft', ArrowRight: 'ArrowRight', ArrowUp: 'ArrowUp', ArrowDown: 'ArrowDown', KeyR: 'r', KeyC: 'c', KeyJ: 'j', ShiftLeft: 'Shift' };
      const send = (type, code) => {
        const ev = { code, key: keyOf[code] || code, bubbles: true, cancelable: true, composed: true };
        canvas.dispatchEvent(new KeyboardEvent(type, ev));
      };
      document.querySelectorAll('#mm-touch button').forEach((b) => {
        const code = b.dataset.code;
        const down = (e) => { e.preventDefault(); if (b.classList.contains('on')) return; b.classList.add('on'); send('keydown', code); };
        const up = (e) => { e.preventDefault(); if (!b.classList.contains('on')) return; b.classList.remove('on'); send('keyup', code); };
        b.addEventListener('pointerdown', down);
        b.addEventListener('pointerup', up);
        b.addEventListener('pointercancel', up);
        b.addEventListener('pointerleave', up);
        b.addEventListener('contextmenu', (e) => e.preventDefault());
      });
      window.mmSendKey = send;  // for scripted tests
    })();
  </script>
"""


def run(cmd, **kw):
    print('+', ' '.join(cmd) if isinstance(cmd, list) else cmd, flush=True)
    return subprocess.run(cmd, check=True, **kw)


def make_play(src_html):
    html = open(src_html, encoding='utf-8').read()
    assert '</body>' in html and "id=\"bevy\"" in html, 'viewer/web/index.html changed shape; update deploy.py'
    html = html.replace('<title>MarsMaker Buggy</title>', '<title>MarsMaker: drive on Mars</title>')
    return html.replace('</body>', OVERLAY + '</body>', 1)


def make_live(src_html):
    html = open(src_html, encoding='utf-8').read()
    meta = '<meta name="play-page" content="index.html">'
    assert meta in html, 'viewer/web/live.html changed shape; update deploy.py'
    html = html.replace(meta, '<meta name="play-page" content="play.html">', 1)
    # a way back to the curated landing page
    h1 = '<h1>MarsMaker: drive any HiRISE DTM</h1>'
    if h1 in html:
        html = html.replace(h1, '<h1><a href="./" style="color:inherit;text-decoration:none">&larr; MarsMaker</a>: drive any HiRISE DTM</h1>', 1)
    return html


def netlify_cli():
    exe = shutil.which('netlify')
    return [exe] if exe else [shutil.which('npx') or 'npx', '--no-install', 'netlify']


def netlify_site_id(name):
    """Id of the Netlify site called NAME, creating it if it does not exist yet."""
    cli = netlify_cli()
    r = subprocess.run(cli + ['sites:list', '--json'], capture_output=True, text=True, check=True)
    for site in json.loads(r.stdout[r.stdout.index('['):]):
        if site['name'] == name:
            return site['id']
    run(cli + ['sites:create', '--name', name, '--disable-linking'])
    r = subprocess.run(cli + ['sites:list', '--json'], capture_output=True, text=True, check=True)
    for site in json.loads(r.stdout[r.stdout.index('['):]):
        if site['name'] == name:
            return site['id']
    sys.exit(f'could not create Netlify site {name!r} (name taken? try --netlify-site {name}-mars)')


def netlify_deploy(out, paths, name, n_sites):
    """Hard-link exactly PATHS (plus netlify.toml) into OUT/_netlify and deploy that to production."""
    stage = os.path.join(out, '_netlify')
    if os.path.exists(stage):
        shutil.rmtree(stage)
    for p in paths:
        full = os.path.join(out, p)
        files = [full] if os.path.isfile(full) else [os.path.join(r, f) for r, _, fs in os.walk(full) for f in fs]
        for f in files:
            dst = os.path.join(stage, os.path.relpath(f, out))
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            try:
                os.link(f, dst)
            except OSError:
                shutil.copy2(f, dst)
    shutil.copy(os.path.join(HERE, 'netlify.toml'), os.path.join(stage, 'netlify.toml'))
    site = netlify_site_id(name)
    run(netlify_cli() + ['deploy', '--prod', '--dir', '.', '--site', site, '--no-build',
                         '--message', f'{n_sites} sites'], cwd=stage)
    print(f'Netlify: https://{name}.netlify.app/')


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--pkg', default=r'D:\marsmaker_data\web\pkg', help='wasm-bindgen output dir')
    ap.add_argument('--out', default=r'D:\marsmaker_data\pages_build')
    ap.add_argument('--no-pack', action='store_true', help='reuse existing sites/, thumbs/, sites.json')
    ap.add_argument('--no-push', action='store_true', help='do not publish to GitHub Pages')
    ap.add_argument('--netlify', action='store_true', help='also deploy to Netlify (netlify-cli, logged in)')
    ap.add_argument('--netlify-site', default='marsmaker', help='Netlify site name (created if missing)')
    ap.add_argument('--remote', default='https://github.com/zombodotcom/marsmaker.git')
    ap.add_argument('--repo', default='zombodotcom/marsmaker')
    a = ap.parse_args()
    out = a.out

    if not a.no_pack:
        run([sys.executable, os.path.join(HERE, 'pack_sites.py'), '--out', out])
    sites = json.load(open(os.path.join(out, 'sites.json')))['sites']

    shutil.copy(os.path.join(HERE, 'index.html'), os.path.join(out, 'index.html'))
    with open(os.path.join(out, 'play.html'), 'w', encoding='utf-8', newline='\n') as f:
        f.write(make_play(os.path.join(REPO, 'viewer', 'web', 'index.html')))
    with open(os.path.join(out, 'live.html'), 'w', encoding='utf-8', newline='\n') as f:
        f.write(make_live(os.path.join(REPO, 'viewer', 'web', 'live.html')))
    shutil.copy(os.path.join(REPO, 'viewer', 'web', 'live_index.json'), os.path.join(out, 'live_index.json'))
    pkg = os.path.join(out, 'pkg')
    if os.path.exists(pkg):
        shutil.rmtree(pkg)
    shutil.copytree(a.pkg, pkg)
    open(os.path.join(out, '.nojekyll'), 'w').close()

    paths = ['index.html', 'play.html', 'live.html', 'live_index.json', 'sites.json', '.nojekyll', 'pkg']
    paths += [f'sites/{s["site"]}' for s in sites] + [f'thumbs/{s["site"]}.jpg' for s in sites]
    total, biggest = 0, (0, '')
    for p in paths:
        full = os.path.join(out, p)
        files = [full] if os.path.isfile(full) else [os.path.join(r, f) for r, _, fs in os.walk(full) for f in fs]
        for f in files:
            n = os.path.getsize(f)
            total += n
            biggest = max(biggest, (n, f))
    print(f'{len(sites)} sites, {total / 1e6:.0f} MB total, largest file {biggest[0] / 1e6:.1f} MB ({biggest[1]})')
    assert biggest[0] < 100e6, 'a file is over the 100 MB GitHub limit'
    assert total < 950e6, 'deploy is over ~950 MB (GitHub Pages limit is 1 GB)'
    if a.netlify:
        netlify_deploy(out, paths, a.netlify_site, len(sites))
    if a.no_push:
        return

    gitdir = os.path.join(out, '_publish.git')
    if os.path.exists(gitdir):
        shutil.rmtree(gitdir, onerror=lambda f, p, e: (os.chmod(p, 0o666), f(p)))
    g = ['git', f'--git-dir={gitdir}', f'--work-tree={out}']
    run(['git', 'init', '-q', '--bare', gitdir])
    run(g + ['config', 'core.bare', 'false'])
    run(g + ['config', 'core.autocrlf', 'false'])
    run(g + ['symbolic-ref', 'HEAD', 'refs/heads/gh-pages'])
    run(g + ['add', '-f', '--'] + paths)
    msg = f'Pages build: {len(sites)} sites, {total / 1e6:.0f} MB'
    run(g + ['-c', 'user.name=zombodotcom', '-c', 'user.email=zombodotcom@users.noreply.github.com',
             'commit', '-q', '-m', msg])
    run(g + ['-c', 'credential.helper=', '-c', 'credential.helper=!gh auth git-credential',
             '-c', 'http.postBuffer=1048576000', 'push', '-f', a.remote, 'gh-pages:gh-pages'])

    # Pages from gh-pages / root (create, or update if it already exists)
    body = ['-f', 'source[branch]=gh-pages', '-f', 'source[path]=/']
    r = subprocess.run(['gh', 'api', '-X', 'POST', f'repos/{a.repo}/pages'] + body, capture_output=True, text=True)
    if r.returncode:
        print('pages create:', (r.stdout + r.stderr).strip()[:300])
        run(['gh', 'api', '-X', 'PUT', f'repos/{a.repo}/pages'] + body)
    run(['gh', 'api', f'repos/{a.repo}/pages'])
    print('\nPublished. Check progress with: gh api repos/%s/pages/builds/latest' % a.repo)


if __name__ == '__main__':
    main()

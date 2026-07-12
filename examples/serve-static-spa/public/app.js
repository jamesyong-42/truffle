// A tiny History-API single-page app served at `/`, calling its API at `/api`
// — both published on ONE tailnet origin by mesh.serve({ routes }). Two things
// to notice:
//   • /api needs no CORS and no second host: it's the same origin, longest-
//     prefix-routed to a local backend by the sidecar.
//   • Deep links survive a refresh: /about has no file, so mesh.serve's
//     `fallback` serves index.html and the router below renders the view.

async function homeView() {
  let api;
  try {
    const res = await fetch('/api/hello');
    api = JSON.stringify(await res.json(), null, 2);
  } catch (err) {
    api = `error: ${err}`;
  }
  return `<h1>Home</h1>
    <p>This page is static; the box below is a live call to <code>/api/hello</code>,
    reverse-proxied to a local backend on the same origin — no CORS, no second host.</p>
    <pre>${api}</pre>`;
}

const staticViews = {
  '/about':
    '<h1>About</h1><p>Refresh this page. It still works — the server had no <code>/about</code> file, so the SPA <code>fallback</code> served <code>index.html</code> and the router rendered this view.</p>',
};

async function render(path) {
  const view = document.getElementById('view');
  view.innerHTML = path === '/' ? await homeView() : (staticViews[path] ?? '<h1>404</h1>');
}

document.addEventListener('click', (e) => {
  const link = e.target.closest('a[data-link]');
  if (!link) return;
  e.preventDefault();
  history.pushState({}, '', link.getAttribute('href'));
  render(location.pathname);
});

window.addEventListener('popstate', () => render(location.pathname));
render(location.pathname);

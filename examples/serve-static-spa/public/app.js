// A tiny History-API single-page app. It exists to show why `fallback` matters:
// deep links like /about are handled here in the browser, but a hard refresh
// asks the *server* for /about. There's no such file — so `mesh.serve`'s
// `fallback: '/index.html'` serves this page instead, and the router below
// renders the right view. Without the fallback, that refresh would 404.

const views = {
  '/': '<h1>Home</h1><p>This whole site is a static directory published with <code>mesh.serve({ dir, fallback })</code>.</p>',
  '/about':
    '<h1>About</h1><p>Refresh this page. It still works — the server missed <code>/about</code> and the SPA fallback served <code>index.html</code>.</p>',
};

function render(path) {
  document.getElementById('view').innerHTML = views[path] ?? '<h1>404</h1>';
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

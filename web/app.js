const dialog = document.querySelector('#provider-dialog');
const form = document.querySelector('#provider-form');
const errorBox = document.querySelector('#form-error');

for (const id of ['open-dialog']) document.querySelector(`#${id}`).addEventListener('click', () => dialog.showModal());
for (const id of ['close-dialog', 'cancel-dialog']) document.querySelector(`#${id}`).addEventListener('click', () => dialog.close());

async function loadProviders() {
  const response = await fetch('/admin/providers');
  const providers = await response.json();
  document.querySelector('#provider-count').textContent = `${providers.length} provider${providers.length === 1 ? '' : 's'}`;
  document.querySelector('#empty').hidden = providers.length > 0;
  document.querySelector('#providers-table').hidden = providers.length === 0;
  const tbody = document.querySelector('#providers');
  tbody.replaceChildren(...providers.map(provider => {
    const row = document.createElement('tr');
    const values = [provider.name, provider.kind.replace('_', ' '), provider.base_url, provider.models.join(', ') || 'All'];
    values.forEach((value, index) => {
      const cell = document.createElement('td');
      if (index === 1) { const badge = document.createElement('span'); badge.className = 'kind'; badge.textContent = value; cell.append(badge); }
      else cell.textContent = value;
      row.append(cell);
    });
    const action = document.createElement('td');
    const button = document.createElement('button');
    button.className = 'delete'; button.textContent = 'Delete';
    button.addEventListener('click', async () => {
      if (!confirm(`Delete ${provider.name}?`)) return;
      await fetch(`/admin/providers/${encodeURIComponent(provider.id)}`, { method: 'DELETE' });
      loadProviders();
    });
    action.append(button); row.append(action); return row;
  }));
}

form.addEventListener('submit', async event => {
  event.preventDefault(); errorBox.textContent = '';
  const fields = new FormData(form);
  const payload = Object.fromEntries(fields);
  payload.models = payload.models.split(',').map(value => value.trim()).filter(Boolean);
  const response = await fetch('/admin/providers', {
    method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(payload)
  });
  if (!response.ok) {
    const body = await response.json().catch(() => ({}));
    errorBox.textContent = body.error?.message || 'Could not add provider'; return;
  }
  form.reset(); dialog.close(); await loadProviders();
});

loadProviders().catch(() => { document.querySelector('#empty p').textContent = 'Could not connect to Yabane.'; });

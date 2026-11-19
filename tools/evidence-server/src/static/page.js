// The map and list page. Loaded from the server itself; the server's
// Content-Security-Policy allows no inline script and no other script
// source. Every value from the server goes through esc() before innerHTML.
'use strict';
const $ = (id) => document.getElementById(id);
let map = null, layer = null, units = [];
try { $('actor').value = localStorage.getItem('actor') || ''; } catch (e) {}
function headers() {
  const h = { 'X-Actor': $('actor').value || 'anonymous' };
  if ($('token').value) h['Authorization'] = 'Bearer ' + $('token').value;
  return h;
}
function esc(s) { return String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c])); }
function num(x, d) { return typeof x === 'number' && isFinite(x) ? x.toFixed(d) : '?'; }
async function devices() {
  const r = await fetch('/api/devices', { headers: headers() });
  if (!r.ok) { $('rows').innerHTML = '<tr><td colspan=6>' + esc(r.status) + ' (token?)</td></tr>'; return; }
  units = await r.json();
  $('dev').innerHTML = units.map(d => `<option value="${esc(d.key_id)}">${esc(d.name)} (${esc(d.key_id)}, ${esc(d.detections)} detections)</option>`).join('');
}
// What the server knows is wrong or weak about this unit's stored log.
function warnings(id) {
  const d = units.find(u => u.key_id === id);
  const w = [];
  if (d && d.test_key) w.push('This unit signs with the PUBLISHED TEST KEY: anyone can make records like these. Not evidence.');
  if (d && Array.isArray(d.problems)) for (const p of d.problems) w.push('Stored files: ' + p);
  $('warn').hidden = w.length === 0;
  $('warn').innerHTML = w.length ? '<ul>' + w.map(x => '<li>' + esc(x) + '</li>').join('') + '</ul>' : '';
}
async function show() {
  try { localStorage.setItem('actor', $('actor').value); } catch (e) {}
  const id = $('dev').value; if (!id) return;
  warnings(id);
  const r = await fetch(`/api/devices/${encodeURIComponent(id)}/detections`, { headers: headers() });
  if (!r.ok) { $('rows').innerHTML = '<tr><td colspan=6>' + esc(r.status) + '</td></tr>'; return; }
  const pts = await r.json();
  if (!map && window.L) {
    map = L.map('map');
    L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', { maxZoom: 19, attribution: '&copy; OpenStreetMap' }).addTo(map);
  }
  const bounds = [];
  if (map) {
    if (layer) layer.remove();
    layer = L.layerGroup().addTo(map);
    for (const p of pts) {
      if (typeof p.lat === 'number' && typeof p.lon === 'number') { L.circleMarker([p.lat, p.lon], { radius: 5, color: '#d33' }).bindPopup(`#${esc(p.seq)} ${esc(p.drone_id)}<br>${esc(p.summary)}`).addTo(layer); bounds.push([p.lat, p.lon]); }
      if (typeof p.hub_lat === 'number' && typeof p.hub_lon === 'number') { L.circleMarker([p.hub_lat, p.hub_lon], { radius: 4, color: '#06f' }).bindPopup('hub (' + esc(p.hub_position) + ')').addTo(layer); bounds.push([p.hub_lat, p.hub_lon]); }
    }
    if (bounds.length) map.fitBounds(bounds, { maxZoom: 16 }); else map.setView([39.95, -75.16], 10);
  }
  $('rows').innerHTML = pts.slice().reverse().slice(0, 500).map(p => `<tr><td>${esc(p.seq)}</td><td>${p.utc_ms ? esc(new Date(p.utc_ms).toISOString()) : 'NO UTC'} (${esc(p.time_source)})</td><td>${esc(p.drone_id)}</td><td>${p.lat != null ? num(p.lat, 6) + ', ' + num(p.lon, 6) : '-'}</td><td>${p.hub_lat != null ? num(p.hub_lat, 6) + ', ' + num(p.hub_lon, 6) + ' (' + esc(p.hub_position) + ')' : 'NO POSITION'}</td><td class="s">${esc(p.summary)}</td></tr>`).join('');
  $('export').onclick = async (e) => {
    e.preventDefault();
    const r = await fetch(`/api/devices/${encodeURIComponent(id)}/export`, { headers: headers() });
    if (!r.ok) { alert('Export failed: ' + r.status); return; }
    const blob = await r.blob();
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob); a.download = `evidence-${id}.json`; a.click();
    alert('Bundle SHA-256: ' + r.headers.get('x-bundle-sha256'));
  };
}
$('load').onclick = show;
devices();

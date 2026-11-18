// Keep the Android score: ping 40%, logarithmic download 40%, upload 20%.
export function serverScore(result) {
  if (!result?.ok || !Number.isFinite(result.pingMs)) return null
  const parts = [[1 - Math.min(1, Math.max(0, (result.pingMs - 30) / 570)), 0.4]]
  for (const [key, top, weight] of [['downloadMbps', 300, 0.4], ['uploadMbps', 150, 0.2]]) {
    if (Number.isFinite(result[key])) parts.push([Math.min(1, Math.log1p(Math.max(0, result[key])) / Math.log1p(top)), weight])
  }
  return Math.round(parts.reduce((s, [v, w]) => s + v * w, 0) / parts.reduce((s, [, w]) => s + w, 0) * 100)
}

export function rankProfiles(names, results) {
  const byName = new Map(results.map(r => [r.name, r]))
  if (!names.some(name => serverScore(byName.get(name)) !== null)) return [...names]
  return [...names].sort((a, b) => {
    const x = byName.get(a), y = byName.get(b)
    const sx = serverScore(x), sy = serverScore(y)
    const category = (r, score) => score !== null ? 0 : r?.status === 'fail' ? 2 : 1
    return category(x, sx) - category(y, sy) || (sy ?? -1) - (sx ?? -1) || (sx !== null && sy !== null ? x.pingMs - y.pingMs : 0)
  })
}

export function loadMeasurements() {
  try {
    const stored = JSON.parse(localStorage.getItem('serverMeasurements') || '[]')
    return Array.isArray(stored) ? stored.filter(r => typeof r?.name === 'string' && typeof r.ok === 'boolean') : []
  } catch { return [] }
}

// A ping-only refresh (or an untested item after cancellation) keeps the last speed test.
export function mergeMeasurement(result, previous, preserveSpeed) {
  return {...result, ...(preserveSpeed ? {
    downloadMbps: previous?.downloadMbps, uploadMbps: previous?.uploadMbps,
    speedError: previous?.speedError, speedMeasuredAt: previous?.speedMeasuredAt,
  } : {}), status: result.ok ? 'success' : 'fail'}
}

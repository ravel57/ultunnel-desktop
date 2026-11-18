import test from 'node:test'
import assert from 'node:assert/strict'
import { serverScore, rankProfiles, loadMeasurements, mergeMeasurement } from '../src/serverRating.js'

test('Android score weights, clamping and missing speed components', () => {
  const score = values => serverScore({ok: true, ...values})
  assert.equal(score({pingMs: 30}), 100)
  assert.equal(score({pingMs: 315}), 50)
  assert.equal(score({pingMs: 600}), 0)
  assert.equal(score({pingMs: 600, downloadMbps: 300, uploadMbps: 150}), 60)
  assert.equal(score({pingMs: 600, downloadMbps: 300}), 50)
  assert.equal(score({pingMs: 0, downloadMbps: 600, uploadMbps: 300}), 100)
  assert.equal(score({pingMs: 600, downloadMbps: -1}), 0)
  assert.equal(serverScore({ok: false, pingMs: 30}), null)
  assert.equal(score({}), null)
})

test('ranking uses score, then ping, retains unknown order, puts failures last', () => {
  const names = ['failed', 'unknown-b', 'slow', 'unknown-a', 'fast', 'tie']
  const results = [
    {name: 'failed', ok: false, status: 'fail', pingMs: 1},
    {name: 'slow', ok: true, pingMs: 315},
    {name: 'fast', ok: true, pingMs: 600, downloadMbps: 300, uploadMbps: 150},
    {name: 'tie', ok: true, pingMs: 315},
  ]
  assert.deepEqual(rankProfiles(names, results), ['fast', 'slow', 'tie', 'unknown-b', 'unknown-a', 'failed'])
  assert.deepEqual(rankProfiles(names, []), names)
  assert.deepEqual(names, ['failed', 'unknown-b', 'slow', 'unknown-a', 'fast', 'tie'])
})

test('corrupt persistence cannot break profile selection', () => {
  globalThis.localStorage = {getItem: () => '{bad json'}
  assert.deepEqual(loadMeasurements(), [])
  localStorage.getItem = () => '{"name":"wrong shape"}'
  assert.deepEqual(loadMeasurements(), [])
  localStorage.getItem = () => '[null, {"name":"server", "ok":true, "pingMs":30}]'
  assert.equal(loadMeasurements().length, 1)
})

test('ping refresh keeps speed and its timestamp; failed speed clears old numbers', () => {
  const previous = {name: 'server', ok: true, pingMs: 50, downloadMbps: 200, uploadMbps: 100, speedMeasuredAt: 123}
  const ping = {name: 'server', ok: true, pingMs: 60, downloadMbps: null, uploadMbps: null, speedMeasuredAt: null}
  const refreshed = mergeMeasurement(ping, previous, true)
  assert.equal(refreshed.pingMs, 60)
  assert.equal(refreshed.downloadMbps, 200)
  assert.equal(refreshed.speedMeasuredAt, 123)
  const failedSpeed = mergeMeasurement({...ping, speedMeasuredAt: 456, speedError: 'unavailable'}, previous, false)
  assert.equal(failedSpeed.downloadMbps, null)
  assert.equal(failedSpeed.speedMeasuredAt, 456)
  assert.equal(failedSpeed.status, 'success')
  assert.notEqual(serverScore(failedSpeed), null)
})

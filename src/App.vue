<template>
	<div class="app">
		<!-- Header -->
		<div class="topbar">
			<div class="title">{{ activeTab === 'control' ? 'Панель управления' : 'Настройки' }}</div>
		</div>

		<!-- CONTROL TAB -->
		<div v-if="activeTab === 'control'" class="page">
			<div v-if="isRunning" class="card">
				<div class="card-title">Статистика</div>

				<div class="statsGrid">
					<div class="statItem">
						<div class="statLabel">Скачивание</div>
						<div class="statValue">{{ formatSpeed(dashboard.downBps) }}</div>
					</div>

					<div class="statItem">
						<div class="statLabel">Отдача</div>
						<div class="statValue">{{ formatSpeed(dashboard.upBps) }}</div>
					</div>

					<div class="statItem">
						<div class="statLabel">Соединения</div>
						<div class="statValue">{{ dashboard.activeConnections }}</div>
					</div>

					<div class="statItem">
						<div class="statLabel">Память</div>
						<div class="statValue">{{ dashboard.memoryMb }} MB</div>
					</div>

					<div class="statItem">
						<div class="statLabel">Версия core</div>
						<div class="statValue">{{ dashboard.coreVersion }}</div>
					</div>
				</div>
			</div>

			<div v-if="isRunning" class="card">
				<div class="card-title">График трафика</div>
				<TrafficChart :items="trafficHistory"/>
			</div>

			<div class="card">
				<div class="row-between profileCheckHead">
					<div>
						<div class="card-title">Профиль</div>
					</div>
					<button class="btn btn-ghost" @click="checkProfiles" :disabled="checkingProfiles || loadingProfiles || !profiles.length">
						{{ checkingProfiles ? `Проверка ${profileCheckProgress} / ${profileCheckTotal || profiles.length}` : 'Проверить профили' }}
					</button>
				</div>

				<div v-if="loadingProfiles" class="muted">Загрузка…</div>

				<div v-else class="list profileList">
					<label
						v-for="name in profiles"
						:key="name"
						class="row profileRow"
						:class="profileRowClass(name)"
					>
						<div class="profileRowMain">
							<input
								class="radio"
								type="radio"
								name="profile"
								:value="name"
								v-model="selectedProfile"
								@change="onSelectProfile(name)"
							/>
							<span class="row-text">{{ name }}</span>
						</div>

						<div v-if="shouldShowProfileCheckState(name)" class="profileCheckInline">
							<span class="checkBadge">{{ checkStatusLabel(getProfileCheckResult(name)) }}</span>
							<span class="checkValue">{{ checkResultText(getProfileCheckResult(name)) }}</span>
						</div>
					</label>

					<div v-if="!profiles.length" class="muted">
						Профили не загружены. Нажмите «Обновить конфиги» в настройках.
					</div>
				</div>

			</div>
		</div>

		<!-- SETTINGS TAB -->
		<div v-else class="page">
			<!-- Access Key -->
			<div class="card">
				<div class="card-title">Ключ доступа</div>

				<input
					class="input"
					type="text"
					placeholder="Введите ключ доступа…"
					v-model="accessKey"
					@blur="saveAccessKey"
					@keyup.enter="saveAccessKey"
				/>

				<div class="actions">
					<button class="btn" @click="refreshConfigs" :disabled="loadingConfigs">
						{{ loadingConfigs ? 'Обновление…' : 'Обновить конфиги' }}
					</button>
				</div>

				<div v-if="errorText" class="error">{{ errorText }}</div>
			</div>

			<!-- Logs -->
			<div class="card">
				<div class="row-between">
					<div>
						<div class="card-title">Логи</div>
						<div class="muted">Откроется файл логов приложения</div>
					</div>
					<button class="btn btn-ghost" @click="openLogs">Открыть</button>
				</div>
			</div>

			<div class="card">
				<div class="card-title">Приложение</div>

				<label class="row">
					<input
						type="checkbox"
						v-model="autostartEnabled"
						:disabled="autostartLoading"
						@change="saveAutostart"
					/>
					<span>Автозапуск при старте системы</span>
				</label>

				<div class="muted" style="margin-top:6px" v-if="autostartNote">
					{{ autostartNote }}
				</div>
			</div>

			<!-- apps settings -->
			<div class="card">
				<div class="card-title">Маршрутизация</div>

				<label class="row">
					<input type="checkbox" v-model="split.enabled" @change="saveSplit"/>
					<span>Раздельная маршрутизация</span>
				</label>

				<div class="splitBlock">
					<div class="smallTitle">Пускать через прокси (apps → proxy)</div>
					<div class="row">
						<button class="btn btn-ghost" @click="openAppsPicker('proxyApps')">Выбрать из запущенных
						</button>
					</div>
					<div class="chips">
							<span class="chip" v-for="a in split.proxyApps" :key="a">
							{{ a }} <button class="chipX" @click="removeFrom('proxyApps', a)">×</button>
							</span>
					</div>
					<div class="sep"></div>
				</div>

				<div class="splitBlock">
					<div class="smallTitle">Пускать через прокси (domains → proxy)</div>
					<div class="row">
						<input class="input" v-model="newProxyDomain" placeholder="например: google.com"/>
						<button class="btn" @click="addTo('proxyDomains','newProxyDomain')">Добавить</button>
					</div>
					<div class="chips">
							<span class="chip" v-for="d in split.proxyDomains" :key="d">
								{{ d }} <button class="chipX" @click="removeFrom('proxyDomains', d)">×</button>
							</span>
					</div>
					<div class="sep"></div>
				</div>

				<div class="splitBlock">
					<label class="row">
						<input
							type="checkbox"
							v-model="socks5Inbound"
							:disabled="!split.enabled"
							@change="saveSocks5Inbound"
						/>
						<span>Включить браузерный прокси</span>
					</label>
					<div class="muted" v-if="!split.enabled" style="margin-top:6px">
						Доступно только при включенной «Раздельной маршрутизации».
					</div>
				</div>
			</div>

		</div>

		<!-- Floating Start/Stop button (like play button) -->
		<button
			class="fab"
			:class="{ on: isRunning }"
			@click="toggleRun"
			:title="isRunning ? 'Остановить' : 'Запустить'"
		>
			<span class="fabIcon" v-if="!isRunning">▶</span>
			<span class="fabIcon" v-else>■</span>
		</button>

		<!-- Bottom nav -->
		<div class="bottom-nav">
			<button
				class="nav-btn"
				:class="{ active: activeTab === 'control' }"
				@click="activeTab = 'control'"
			>
				<div class="nav-ico">▦</div>
				<div class="nav-txt">Панель управления</div>
			</button>

			<button
				class="nav-btn"
				:class="{ active: activeTab === 'settings' }"
				@click="activeTab = 'settings'"
			>
				<div class="nav-ico">⚙</div>
				<div class="nav-txt">Настройки</div>
			</button>
		</div>
	</div>

	<!-- Apps Picker Modal -->
	<div v-if="appsModalOpen" class="modalOverlay" @click.self="closeAppsPicker">
		<div class="modal">
			<div class="modalHead">
				<div class="modalTitle">Запущенные приложения</div>
				<button class="btn btn-ghost" @click="closeAppsPicker">✕</button>
			</div>

			<div class="row">
				<input class="input" v-model="appsSearch" placeholder="Поиск: имя / путь / заголовок окна…"/>
				<button class="btn" @click="refreshRunningApps" :disabled="appsLoading">
					{{ appsLoading ? 'Обновление…' : 'Обновить' }}
				</button>
			</div>

			<div class="muted" v-if="appsLoading">Загрузка…</div>

			<div class="appsList" v-else>
				<button
					v-for="a in filteredRunningApps"
					:key="a.pid + ':' + (a.path || a.name)"
					class="appRow"
					@click="addRunningApp(a)"
				>
					<div class="appMain">
						<div class="appName">{{ a.name }}</div>
						<div class="appTitle" v-if="a.title">{{ a.title }}</div>
					</div>
					<div class="appPath">{{ a.path || '—' }}</div>
				</button>

				<div class="muted" v-if="!filteredRunningApps.length">
					Ничего не найдено
				</div>
			</div>

			<div class="modalHint muted">
				При выборе добавляется <b>путь</b> (process_path). Если путь недоступен — добавится имя процесса
				(process_name).
			</div>
		</div>
	</div>

</template>

<script lang="ts">
import {defineComponent, h} from 'vue'
import {invoke} from '@tauri-apps/api/core'
import {listen, type UnlistenFn} from '@tauri-apps/api/event'

type SplitListKey = "bypassApps" | "proxyApps" | "bypassDomains" | "proxyDomains";
type InputKey = "newBypassApp" | "newProxyApp" | "newBypassDomain" | "newProxyDomain";

type TrafficPoint = {
	time: string
	up: number
	down: number
}

type DashboardStats = {
	upBps: number
	downBps: number
	activeConnections: number
	memoryMb: number
	coreVersion?: string | null
}

type SplitRoutingSettings = {
	enabled: boolean
	bypassApps: string[]
	proxyApps: string[]
	bypassDomains: string[]
	proxyDomains: string[]
	proxyOutbound: string
	directOutbound: string
}

type RunningApp = {
	pid: number
	name: string
	path?: string | null
	title?: string | null
}

type ProfileCheckStatus = 'pending' | 'checking' | 'success' | 'fail'

type ProfileCheckResult = {
	name: string
	ok: boolean
	ip?: string | null
	error?: string | null
	status?: ProfileCheckStatus
}

type ProfileCheckProgressEvent = {
	name: string
	index: number
	total: number
	status: 'checking' | 'success' | 'fail' | 'finished'
	ip?: string | null
	error?: string | null
}

function defaultSplit(): SplitRoutingSettings {
	return {
		enabled: false,
		bypassApps: [],
		proxyApps: [],
		bypassDomains: [],
		proxyDomains: [],
		proxyOutbound: "proxy",
		directOutbound: "direct",
	}
}

const TrafficChart = defineComponent({
	name: 'TrafficChart',
	props: {
		items: {
			type: Array as () => TrafficPoint[],
			required: true,
		},
	},
	methods: {
		buildPath(values: number[], width: number, height: number, maxValue: number): string {
			if (!values.length) return ''

			const safeMax = Math.max(maxValue, 1)
			const stepX = values.length > 1 ? width / (values.length - 1) : width

			return values
				.map((value, index) => {
					const x = index * stepX
					const y = height - (value / safeMax) * height
					return `${index === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`
				})
				.join(' ')
		},
	},
	render() {
		const items = this.items || []
		const width = 640
		const height = 220

		if (!items.length) {
			return h('div', {class: 'chartEmpty'}, 'Нет данных')
		}

		const upValues = items.map(x => Number(x.up || 0))
		const downValues = items.map(x => Number(x.down || 0))
		const maxValue = Math.max(1, ...upValues, ...downValues)

		const upPath = this.buildPath(upValues, width, height, maxValue)
		const downPath = this.buildPath(downValues, width, height, maxValue)

		const grid = [0.25, 0.5, 0.75].map(ratio =>
			h('line', {
				x1: 0,
				y1: height * ratio,
				x2: width,
				y2: height * ratio,
				stroke: 'rgba(255,255,255,0.10)',
				'stroke-width': 1,
			})
		)

		return h('div', {class: 'chartWrap'}, [
			h('svg', {
				viewBox: `0 0 ${width} ${height}`,
				class: 'trafficSvg',
				preserveAspectRatio: 'none',
			}, [
				...grid,
				h('path', {
					d: downPath,
					fill: 'none',
					stroke: '#60a5fa',
					'stroke-width': 3,
					'stroke-linejoin': 'round',
					'stroke-linecap': 'round',
				}),
				h('path', {
					d: upPath,
					fill: 'none',
					stroke: '#c084fc',
					'stroke-width': 3,
					'stroke-linejoin': 'round',
					'stroke-linecap': 'round',
				}),
			]),
			h('div', {class: 'chartLegend'}, [
				h('div', {class: 'legendItem'}, [
					h('span', {class: 'legendDot down'}),
					h('span', null, 'Down'),
				]),
				h('div', {class: 'legendItem'}, [
					h('span', {class: 'legendDot up'}),
					h('span', null, 'Up'),
				]),
			]),
		])
	},
})

export default defineComponent({
	name: 'App',

	components: {
		TrafficChart,
	},

	data: () => ({
		activeTab: 'control' as 'control' | 'settings',

		isRunning: false,

		dashboard: {
			upBps: 0,
			downBps: 0,
			activeConnections: 0,
			memoryMb: 0,
			coreVersion: '—',
		} as Required<DashboardStats>,

		trafficHistory: [] as TrafficPoint[],
		statsTimer: null as number | null,

		profiles: [] as string[],
		selectedProfile: '' as string,

		accessKey: '' as string,

		loadingProfiles: false,
		loadingConfigs: false,
		checkingProfiles: false,
		profileCheckCurrent: '' as string,
		profileCheckProgress: 0,
		profileCheckTotal: 0,
		profileCheckResults: [] as ProfileCheckResult[],
		profileCheckUnlisten: null as UnlistenFn | null,
		errorText: '' as string,

		// settings UI (пока просто UI, можно потом сохранять)
		showSpeed: 'on',
		autoUpdate: 'on',
		memLimit: 'on',

		coreVersion: '—',
		coreDataSize: '0 B',

		split: defaultSplit(),
		newBypassApp: "" as string,
		newProxyApp: "" as string,
		newBypassDomain: "" as string,
		newProxyDomain: "" as string,

		runningApps: [] as RunningApp[],
		appsLoading: false,
		appsModalOpen: false,
		appsModalTarget: null as null | SplitListKey, // куда добавляем: bypassApps или proxyApps
		appsSearch: "" as string,

		socks5Inbound: false,

		autostartEnabled: false,
		autostartOsEnabled: false,
		autostartLoading: false,
		autostartNote: '' as string,
	}),

	async created() {
		await this.registerProfileCheckEvents()
		await this.bootstrap()
		await this.loadSplit()
		await this.loadSocks5Inbound()
		await this.loadAutostart()
		this.startStatsPolling()
	},

	beforeUnmount() {
		this.stopStatsPolling()
		if (this.profileCheckUnlisten) {
			this.profileCheckUnlisten()
			this.profileCheckUnlisten = null
		}
	},

	methods: {
		async registerProfileCheckEvents() {
			if (this.profileCheckUnlisten) return

			this.profileCheckUnlisten = await listen<ProfileCheckProgressEvent>('profile-check-progress', (event) => {
				const payload = event.payload
				if (!payload) return

				this.profileCheckTotal = Number(payload.total || this.profileCheckTotal || this.profiles.length || 0)
				this.profileCheckProgress = Number(payload.index || this.profileCheckProgress || 0)

				if (payload.status === 'finished') {
					this.profileCheckCurrent = ''
					this.profileCheckProgress = this.profileCheckTotal
					return
				}

				this.profileCheckCurrent = payload.name
				this.upsertProfileCheckResult({
					name: payload.name,
					ok: payload.status === 'success',
					ip: payload.ip ?? null,
					error: payload.error ?? null,
					status: payload.status === 'checking' ? 'checking' : (payload.status === 'success' ? 'success' : 'fail'),
				})
			})
		},

		upsertProfileCheckResult(result: ProfileCheckResult) {
			const idx = this.profileCheckResults.findIndex(x => x.name === result.name)
			if (idx >= 0) {
				this.profileCheckResults.splice(idx, 1, {...this.profileCheckResults[idx], ...result})
			} else {
				this.profileCheckResults.push(result)
			}
		},

		getProfileCheckResult(name: string): ProfileCheckResult {
			return this.profileCheckResults.find(x => x.name === name) || {
				name,
				ok: false,
				ip: null,
				error: null,
				status: 'pending',
			}
		},

		shouldShowProfileCheckState(name: string): boolean {
			return this.checkingProfiles || this.profileCheckResults.some(x => x.name === name)
		},

		profileRowClass(name: string) {
			return this.checkRowClass(this.getProfileCheckResult(name))
		},

		checkRowClass(r: ProfileCheckResult) {
			return {
				ok: r.status === 'success' || (!r.status && r.ok),
				fail: r.status === 'fail' || (!r.status && !r.ok),
				checking: r.status === 'checking',
				pending: r.status === 'pending',
			}
		},

		checkStatusLabel(r: ProfileCheckResult): string {
			if (r.status === 'checking') return 'Проверяется'
			if (r.status === 'pending') return 'Ожидает'
			if (r.status === 'success' || (!r.status && r.ok)) return 'OK'
			return 'Ошибка'
		},

		checkResultText(r: ProfileCheckResult): string {
			if (r.status === 'pending') return 'Ожидает очереди'
			if (r.status === 'checking') return 'Запуск и проверка IP…'
			if (r.status === 'success' || (!r.status && r.ok)) return r.ip || 'Успешно'
			return r.error || 'Ошибка проверки'
		},

		async bootstrap() {
			try {
				this.errorText = ''
				this.loadingProfiles = true

				// состояние
				this.isRunning = await invoke<boolean>('get_state')

				// ключ
				this.accessKey = await invoke<string>('get_access_key')

				// профили из локального кеша (если ты сделал сохранение configs.json)
				const list = await invoke<string[]>('get_profiles')
				this.profiles = Array.isArray(list) ? list : []

				// выбранный профиль
				const selected = await invoke<string | null>('get_selected_profile')
				if (selected) this.selectedProfile = selected

			} catch (e: any) {
				this.errorText = String(e)
			} finally {
				this.loadingProfiles = false
			}
		},

		async toggleRun() {
			try {
				this.errorText = ''
				if (this.isRunning) {
					await invoke('singbox_stop_platform')
					this.isRunning = false
					this.trafficHistory = []
					await this.loadDashboardStats()
					return
				}
				if (!this.selectedProfile) {
					this.errorText = 'Выберите профиль'
					return
				}
				await invoke('singbox_start_platform')
				this.isRunning = true
				this.trafficHistory = []
				await this.loadDashboardStats()
			} catch (e: any) {
				this.errorText = String(e)
				this.isRunning = await invoke<boolean>('get_state').catch(() => false)
			}
		},

		async onSelectProfile(name: string) {
			try {
				this.errorText = ''
				this.selectedProfile = name
				await invoke('set_selected_profile', {profile: name}) // <-- без id
			} catch (e: any) {
				this.errorText = String(e)
			}
		},

		async saveAccessKey() {
			try {
				this.errorText = ''
				const key = (this.accessKey || '').trim()
				await invoke('set_access_key', {key})
			} catch (e: any) {
				this.errorText = String(e)
			}
		},

		async refreshConfigs() {
			try {
				this.errorText = ''
				this.loadingConfigs = true

				// Сохраним ключ перед загрузкой
				await this.saveAccessKey()

				const list = await invoke<string[]>('load_configs')
				this.profiles = Array.isArray(list) ? list : []

				// если раньше выбранный профиль отсутствует — сбросим
				if (this.selectedProfile && !this.profiles.includes(this.selectedProfile)) {
					this.selectedProfile = ''
					await invoke('set_selected_profile', {profile: ''}).catch(() => {
					})
				}

				// вернём на вкладку управления
				this.activeTab = 'control'
			} catch (e: any) {
				this.errorText = String(e)
			} finally {
				this.loadingConfigs = false
			}
		},

		async checkProfiles() {
			try {
				this.errorText = ''
				this.checkingProfiles = true
				this.profileCheckCurrent = ''
				this.profileCheckProgress = 0
				this.profileCheckTotal = this.profiles.length
				this.profileCheckResults = this.profiles.map(name => ({
					name,
					ok: false,
					ip: null,
					error: null,
					status: 'pending' as ProfileCheckStatus,
				}))

				const wasRunning = this.isRunning
				this.isRunning = false
				this.trafficHistory = []

				const results = await invoke<ProfileCheckResult[]>('check_profiles')
				if (Array.isArray(results)) {
					this.profileCheckResults = results.map(r => ({
						...r,
						status: r.ok ? 'success' : 'fail',
					}))
				}
				this.profileCheckCurrent = ''
				this.profileCheckProgress = this.profileCheckTotal || this.profileCheckResults.length
				this.isRunning = await invoke<boolean>('get_state').catch(() => wasRunning)
			} catch (e: any) {
				this.errorText = String(e)
				this.isRunning = await invoke<boolean>('get_state').catch(() => false)
			} finally {
				this.checkingProfiles = false
			}
		},

		async openLogs() {
			this.errorText = ''
			try {
				await invoke('open_logs')
			} catch (e: any) {
				this.errorText = String(e)
			}
		},
		async loadSplit(): Promise<void> {
			this.split = (await invoke<SplitRoutingSettings>("get_split_routing")) ?? defaultSplit();
		},

		async saveSplit(): Promise<void> {
			await invoke("set_split_routing", {split: this.split})
			// применить сразу
			if (this.isRunning) {
				await invoke("singbox_stop_platform")
				await invoke("singbox_start_platform")
				this.isRunning = true
			}
		},

		addTo(listName: SplitListKey, valueField: InputKey): void {
			const v = (this[valueField] || "").trim();
			if (!v) {
				return;
			}
			if (!this.split[listName].includes(v)) {
				this.split[listName].push(v);
			}
			this[valueField] = "";
			void this.saveSplit();
		},

		removeFrom(listName: SplitListKey, item: string): void {
			this.split[listName] = this.split[listName].filter((x) => x !== item);
			void this.saveSplit();
		},

		async saveSettings() {
			await invoke("set_access_key", {key: this.accessKey})
			await invoke("set_split_routing", {split: this.split})
			alert("Настройки сохранены")
		},

		checkUpdates() {
			// UI-заглушка
			this.errorText = 'Проверка обновлений: пока не реализовано'
		},

		openPrivacy() {
			// UI-заглушка
			this.errorText = 'Политика конфиденциальности: пока не реализовано'
		},

		openAbout() {
			// UI-заглушка
			this.errorText = 'О приложении: пока не реализовано'
		},

		async openAppsPicker(target: SplitListKey) {
			this.appsModalTarget = target
			this.appsModalOpen = true
			this.appsSearch = ""

			// грузим при открытии
			if (!this.runningApps.length) {
				await this.refreshRunningApps()
			}
		},

		async refreshRunningApps() {
			try {
				this.appsLoading = true
				this.runningApps = await invoke<RunningApp[]>("list_running_apps")
			} finally {
				this.appsLoading = false
			}
		},

		closeAppsPicker() {
			this.appsModalOpen = false
			this.appsModalTarget = null
		},

		addRunningApp(app: RunningApp) {
			if (!this.appsModalTarget) return

			// предпочтительно добавляем PATH (самое надёжное для sing-box: process_path)
			let value =
				(app.path && String(app.path).trim().length ? String(app.path).trim() : "") ||
				(app.name || "").trim()

			if (!value) return

			if (!this.split[this.appsModalTarget].includes(value)) {
				this.split[this.appsModalTarget].push(value)
				void this.saveSplit()
			}
		},

		async loadSocks5Inbound(): Promise<void> {
			this.socks5Inbound = await invoke<boolean>("get_socks5_inbound")
		},

		async saveSocks5Inbound(): Promise<void> {
			await invoke("set_socks5_inbound", {enabled: this.socks5Inbound})

			// применить сразу
			if (this.isRunning) {
				await invoke("singbox_stop_platform")
				await invoke("singbox_start_platform")
				this.isRunning = true
			}
		},

		async loadAutostart() {
			this.autostartNote = ''
			this.autostartLoading = true
			try {
				const st = await invoke<{ desired: boolean; enabled: boolean }>('get_autostart_status')
				const desired = !!st?.desired
				const enabled = !!st?.enabled
				this.autostartEnabled = enabled
				this.autostartOsEnabled = enabled
				if (enabled !== desired) {
					this.autostartNote = desired
						? 'Автозапуск сохранён как включённый, но в системе сейчас выключен.'
						: 'Автозапуск сохранён как выключенный, но в системе сейчас включён.'
				} else {
					this.autostartNote = enabled ? 'В системе: включено.' : 'В системе: выключено.'
				}
			} catch (e: any) {
				this.autostartNote = String(e)
			} finally {
				this.autostartLoading = false
			}
		},

		async saveAutostart() {
			this.autostartNote = ''
			this.autostartLoading = true
			try {
				await invoke<void>('set_autostart_enabled', {enabled: this.autostartEnabled})
				await this.loadAutostart()
			} catch (e: any) {
				this.autostartNote = String(e)
			} finally {
				this.autostartLoading = false
			}
		},

		async loadDashboardStats() {
			try {
				const stats = await invoke<DashboardStats>('get_dashboard_stats')

				this.dashboard.upBps = Number(stats?.upBps || 0)
				this.dashboard.downBps = Number(stats?.downBps || 0)
				this.dashboard.activeConnections = Number(stats?.activeConnections || 0)
				this.dashboard.memoryMb = Number(stats?.memoryMb || 0)
				this.dashboard.coreVersion = stats?.coreVersion || '—'

				const now = new Date().toLocaleTimeString()

				this.trafficHistory.push({
					time: now,
					up: this.dashboard.upBps,
					down: this.dashboard.downBps,
				})

				if (this.trafficHistory.length > 30) {
					this.trafficHistory.shift()
				}
			} catch (e: any) {
				this.errorText = 'Статистика недоступна: ' + String(e)
				this.dashboard.upBps = 0
				this.dashboard.downBps = 0
				this.dashboard.activeConnections = 0
				this.dashboard.memoryMb = 0
				this.dashboard.coreVersion = '—'
			}
		},

		startStatsPolling() {
			this.stopStatsPolling()
			void this.loadDashboardStats()

			this.statsTimer = window.setInterval(() => {
				if (this.activeTab === 'control' && this.isRunning) {
					void this.loadDashboardStats()
				}
			}, 1000)
		},

		stopStatsPolling() {
			if (this.statsTimer !== null) {
				clearInterval(this.statsTimer)
				this.statsTimer = null
			}
		},

		formatSpeed(bytes: number): string {
			if (bytes >= 1024 * 1024) {
				return (bytes / 1024 / 1024).toFixed(2) + ' MB/s'
			}
			if (bytes >= 1024) {
				return (bytes / 1024).toFixed(1) + ' KB/s'
			}
			return bytes + ' B/s'
		},

	},

	computed: {
		profileCheckPercent(): number {
			const total = this.profileCheckTotal || this.profiles.length || 0
			if (!total) return 0
			return Math.min(100, Math.round((this.profileCheckProgress / total) * 100))
		},

		filteredRunningApps(): RunningApp[] {
			const q = (this.appsSearch || "").trim().toLowerCase()
			if (!q) return this.runningApps

			return this.runningApps.filter(a => {
				const n = (a.name || "").toLowerCase()
				const p = (a.path || "").toLowerCase()
				const t = (a.title || "").toLowerCase()
				return n.includes(q) || p.includes(q) || t.includes(q)
			})
		},
	}
})
</script>

<style scoped>
:root {
	color-scheme: dark;
}

* {
	box-sizing: border-box;
}

.app {
	margin: 0;
	padding: 16px 16px 110px;
	height: 100vh;
	overflow-y: auto;
	overflow-x: hidden;
	background:
		radial-gradient(circle at top, rgba(112, 82, 255, 0.10), transparent 28%),
		linear-gradient(180deg, #11131a 0%, #151821 100%);
	color: #f3f5f7;
	font-family: system-ui, -apple-system, Segoe UI, Roboto, Arial, sans-serif;
}

/* Top */
.topbar {
	position: sticky;
	top: 0;
	z-index: 20;
	padding: 4px 0 14px;
	background: linear-gradient(180deg, rgba(17, 19, 26, 0.96) 0%, rgba(17, 19, 26, 0.84) 100%);
	backdrop-filter: blur(10px);
}

.title {
	font-size: 28px;
	font-weight: 800;
	line-height: 1.1;
	letter-spacing: -0.02em;
	color: #ffffff;
}

.page {
	display: flex;
	flex-direction: column;
	gap: 18px;
	width: 100%;
	max-width: 760px;
	margin: 0 auto;
}

.card {
	background: rgba(24, 28, 39, 0.92);
	border-radius: 20px;
	padding: 18px;
	border: 1px solid rgba(255, 255, 255, 0.10);
	box-shadow:
		0 10px 30px rgba(0, 0, 0, 0.28),
		inset 0 1px 0 rgba(255, 255, 255, 0.03);
}

.card-title {
	font-weight: 800;
	font-size: 22px;
	line-height: 1.2;
	margin-bottom: 14px;
	color: #ffffff;
}

.list {
	display: flex;
	flex-direction: column;
	gap: 8px;
}

.row {
	display: flex;
	align-items: center;
	gap: 12px;
	padding: 12px 0;
}

.row-text {
	font-size: 16px;
	line-height: 1.4;
	color: #edf1f7;
}

.profileList {
	gap: 10px;
}

.profileRow {
	justify-content: space-between;
	padding: 12px 14px;
	border-radius: 14px;
	background: transparent;
	border: 1px solid transparent;
	transition: background 0.18s ease, border-color 0.18s ease;
}

.profileRow.ok {
	border-color: rgba(34, 197, 94, 0.32);
	background: rgba(34, 197, 94, 0.10);
}

.profileRow.fail {
	border-color: rgba(239, 68, 68, 0.34);
	background: rgba(239, 68, 68, 0.12);
}

.profileRow.checking {
	border-color: rgba(96, 165, 250, 0.36);
	background: rgba(96, 165, 250, 0.12);
}

.profileRow.pending {
	border-color: rgba(255, 255, 255, 0.10);
	background: rgba(255, 255, 255, 0.04);
}

.profileRowMain {
	display: flex;
	align-items: center;
	gap: 12px;
	min-width: 0;
}

.profileRowMain .row-text {
	font-weight: 700;
	word-break: break-word;
}

.profileCheckInline {
	display: inline-flex;
	align-items: center;
	justify-content: flex-end;
	gap: 10px;
	min-width: 0;
	max-width: 48%;
}

.profileCheckInline .checkValue {
	text-align: right;
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
}

.radio {
	width: 18px;
	height: 18px;
	accent-color: #8b5cf6;
	flex: 0 0 auto;
}

input[type="checkbox"] {
	width: 18px;
	height: 18px;
	accent-color: #4f8cff;
	flex: 0 0 auto;
}

.input {
	width: 100%;
	min-height: 50px;
	outline: none;
	background: #202534;
	color: #f8fafc;
	padding: 13px 15px;
	border-radius: 14px;
	border: 1px solid rgba(255, 255, 255, 0.10);
	font-size: 16px;
	line-height: 1.4;
	transition: border-color 0.18s ease, box-shadow 0.18s ease, background 0.18s ease;
}

.input::placeholder {
	color: rgba(226, 232, 240, 0.45);
}

.input:focus {
	border-color: rgba(96, 165, 250, 0.55);
	box-shadow: 0 0 0 4px rgba(96, 165, 250, 0.12);
	background: #23293a;
}

.actions {
	margin-top: 14px;
	display: flex;
	gap: 12px;
	flex-wrap: wrap;
}

.btn {
	background: linear-gradient(180deg, #4f8cff 0%, #3c74ea 100%);
	color: #ffffff;
	border: none;
	border-radius: 14px;
	padding: 12px 16px;
	cursor: pointer;
	font-size: 15px;
	font-weight: 700;
	line-height: 1.2;
	min-height: 48px;
	transition: transform 0.14s ease, opacity 0.14s ease, filter 0.14s ease;
}

.btn:hover {
	filter: brightness(1.06);
}

.btn:active {
	transform: translateY(1px);
}

.btn:disabled {
	opacity: 0.55;
	cursor: default;
	filter: none;
	transform: none;
}

.btn-ghost {
	background: rgba(255, 255, 255, 0.05);
	color: #f3f5f7;
	border: 1px solid rgba(255, 255, 255, 0.10);
}

.row-between {
	display: flex;
	justify-content: space-between;
	align-items: center;
	gap: 12px;
}

.setting-row {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 12px;
	padding: 12px 0;
}

.setting-label {
	font-size: 15px;
	line-height: 1.4;
	color: #e8edf5;
}

.select {
	background: #202534;
	color: #f8fafc;
	border: 1px solid rgba(255, 255, 255, 0.10);
	border-radius: 12px;
	padding: 10px 12px;
	outline: none;
	min-width: 140px;
	font-size: 15px;
}

.kv {
	display: flex;
	justify-content: space-between;
	padding: 8px 0;
}

.k {
	color: rgba(226, 232, 240, 0.68);
	font-size: 14px;
}

.v {
	color: #f8fafc;
	font-size: 14px;
}

.muted {
	color: rgba(226, 232, 240, 0.72);
	font-size: 14px;
	line-height: 1.45;
}

.error {
	margin-top: 12px;
	padding: 12px 14px;
	border-radius: 14px;
	background: rgba(239, 68, 68, 0.14);
	border: 1px solid rgba(239, 68, 68, 0.34);
	color: #fecaca;
	font-size: 14px;
	line-height: 1.45;
}

.splitBlock {
	display: flex;
	flex-direction: column;
	gap: 10px;
	padding: 14px 0 0;
}

.smallTitle {
	font-size: 15px;
	font-weight: 700;
	color: #f4f7fb;
}

.chips {
	display: flex;
	flex-wrap: wrap;
	gap: 10px;
}

.chip {
	display: inline-flex;
	align-items: center;
	gap: 8px;
	padding: 9px 12px;
	border-radius: 999px;
	background: rgba(79, 140, 255, 0.12);
	border: 1px solid rgba(79, 140, 255, 0.25);
	color: #eaf2ff;
	font-size: 14px;
	line-height: 1.3;
	max-width: 100%;
	word-break: break-word;
}

.chipX {
	border: none;
	background: transparent;
	color: #ffffff;
	cursor: pointer;
	font-size: 16px;
	line-height: 1;
	padding: 0;
}

.sep {
	height: 1px;
	background: rgba(255, 255, 255, 0.08);
	margin-top: 6px;
}


.profileCheckHead {
	align-items: flex-start;
	margin-bottom: 14px;
}

.profileCheckStatus {
	margin-top: 14px;
	padding: 14px;
	border-radius: 16px;
	background: #202534;
	border: 1px solid rgba(96, 165, 250, 0.24);
	display: flex;
	flex-direction: column;
	gap: 8px;
}

.currentProfile {
	font-size: 17px;
	font-weight: 800;
	color: #ffffff;
	word-break: break-word;
}

.progressText {
	font-size: 13px;
	color: rgba(226, 232, 240, 0.72);
}

.progressBar {
	height: 9px;
	border-radius: 999px;
	background: rgba(255, 255, 255, 0.08);
	overflow: hidden;
}

.progressFill {
	height: 100%;
	border-radius: inherit;
	background: linear-gradient(90deg, #60a5fa 0%, #8b5cf6 100%);
	transition: width 0.2s ease;
}

.profileCheckResults {
	margin-top: 14px;
	display: flex;
	flex-direction: column;
	gap: 10px;
}

.checkRow {
	display: grid;
	grid-template-columns: minmax(0, 1fr) minmax(150px, auto);
	gap: 12px;
	align-items: center;
	padding: 12px 14px;
	border-radius: 14px;
	background: #202534;
	border: 1px solid rgba(255, 255, 255, 0.10);
}

.checkRow.ok {
	border-color: rgba(34, 197, 94, 0.32);
	background: rgba(34, 197, 94, 0.10);
}

.checkRow.fail {
	border-color: rgba(239, 68, 68, 0.34);
	background: rgba(239, 68, 68, 0.12);
}

.checkRow.checking {
	border-color: rgba(96, 165, 250, 0.36);
	background: rgba(96, 165, 250, 0.12);
}

.checkRow.pending {
	border-color: rgba(255, 255, 255, 0.10);
	background: rgba(255, 255, 255, 0.04);
}

.checkName {
	font-weight: 800;
	color: #ffffff;
	word-break: break-word;
	display: flex;
	align-items: center;
	gap: 8px;
	flex-wrap: wrap;
}

.checkBadge {
	display: inline-flex;
	align-items: center;
	justify-content: center;
	padding: 4px 8px;
	border-radius: 999px;
	background: rgba(255, 255, 255, 0.08);
	color: rgba(226, 232, 240, 0.86);
	font-size: 12px;
	font-weight: 800;
	line-height: 1;
}

.checkValue {
	color: rgba(226, 232, 240, 0.86);
	font-size: 14px;
	line-height: 1.35;
	word-break: break-word;
	text-align: right;
}

/* Stats */
.statsGrid {
	display: grid;
	grid-template-columns: repeat(2, minmax(0, 1fr));
	gap: 14px;
}

.statItem {
	background: #202534;
	border: 1px solid rgba(255, 255, 255, 0.10);
	border-radius: 18px;
	padding: 16px;
	min-height: 106px;
	display: flex;
	flex-direction: column;
	justify-content: center;
}

.statLabel {
	font-size: 15px;
	color: rgba(226, 232, 240, 0.75);
	margin-bottom: 12px;
}

.statValue {
	font-size: 24px;
	font-weight: 800;
	line-height: 1.2;
	color: #ffffff;
	word-break: break-word;
}

.chartWrap {
	display: flex;
	flex-direction: column;
	gap: 12px;
}

.trafficSvg {
	width: 100%;
	height: 240px;
	display: block;
	background: #202534;
	border: 1px solid rgba(255, 255, 255, 0.10);
	border-radius: 18px;
}

.chartLegend {
	display: flex;
	gap: 18px;
	flex-wrap: wrap;
}

.legendItem {
	display: inline-flex;
	align-items: center;
	gap: 8px;
	font-size: 14px;
	color: rgba(226, 232, 240, 0.82);
}

.legendDot {
	width: 10px;
	height: 10px;
	border-radius: 999px;
	display: inline-block;
}

.legendDot.down {
	background: #60a5fa;
}

.legendDot.up {
	background: #c084fc;
}

.chartEmpty {
	height: 240px;
	display: flex;
	align-items: center;
	justify-content: center;
	background: #202534;
	border: 1px solid rgba(255, 255, 255, 0.10);
	border-radius: 18px;
	color: rgba(226, 232, 240, 0.72);
	font-size: 15px;
}

/* FAB */
.fab {
	position: fixed;
	right: 22px;
	bottom: 96px;
	z-index: 30;
	width: 68px;
	height: 68px;
	border: none;
	border-radius: 22px;
	background: linear-gradient(180deg, #b48cff 0%, #8b5cf6 100%);
	color: #ffffff;
	font-size: 28px;
	cursor: pointer;
	box-shadow: 0 16px 28px rgba(139, 92, 246, 0.35);
	display: flex;
	align-items: center;
	justify-content: center;
}

.fab.on {
	background: linear-gradient(180deg, #ff7b7b 0%, #ef4444 100%);
	box-shadow: 0 16px 28px rgba(239, 68, 68, 0.30);
}

.fabIcon {
	line-height: 1;
}

/* Bottom nav */
.bottom-nav {
	position: fixed;
	left: 0;
	right: 0;
	bottom: 0;
	z-index: 25;
	display: grid;
	grid-template-columns: 1fr 1fr;
	gap: 0;
	padding: 10px 12px calc(10px + env(safe-area-inset-bottom));
	background: rgba(17, 19, 26, 0.94);
	backdrop-filter: blur(12px);
	border-top: 1px solid rgba(255, 255, 255, 0.08);
}

.nav-btn {
	border: none;
	background: transparent;
	color: rgba(226, 232, 240, 0.68);
	padding: 10px 8px;
	border-radius: 14px;
	cursor: pointer;
	display: flex;
	flex-direction: column;
	align-items: center;
	gap: 4px;
	transition: background 0.16s ease, color 0.16s ease;
}

.nav-btn.active {
	background: rgba(79, 140, 255, 0.12);
	color: #ffffff;
}

.nav-ico {
	font-size: 20px;
	line-height: 1;
}

.nav-txt {
	font-size: 13px;
	font-weight: 600;
	line-height: 1.2;
}

/* Modal */
.modalOverlay {
	position: fixed;
	inset: 0;
	z-index: 40;
	background: rgba(7, 10, 16, 0.68);
	backdrop-filter: blur(6px);
	padding: 24px;
	display: flex;
	align-items: center;
	justify-content: center;
}

.modal {
	width: min(980px, 100%);
	max-height: min(88vh, 920px);
	background: #161b25;
	border: 1px solid rgba(255, 255, 255, 0.10);
	border-radius: 22px;
	box-shadow: 0 18px 42px rgba(0, 0, 0, 0.40);
	overflow: hidden;
	display: flex;
	flex-direction: column;
}

.modalHead {
	display: flex;
	align-items: center;
	justify-content: space-between;
	padding: 18px 20px;
	border-bottom: 1px solid rgba(255, 255, 255, 0.08);
	background: rgba(255, 255, 255, 0.02);
}

.modalTitle {
	font-weight: 800;
	font-size: 18px;
	color: #ffffff;
}

.appsList {
	padding: 16px;
	display: flex;
	flex-direction: column;
	gap: 12px;
	overflow-y: auto;
	min-height: 0;
}

.appRow {
	text-align: left;
	border: 1px solid rgba(255, 255, 255, 0.10);
	background: #1e2431;
	border-radius: 16px;
	padding: 14px 16px;
	cursor: pointer;
	display: flex;
	flex-direction: column;
	gap: 8px;
	transition: background 0.16s ease, border-color 0.16s ease;
}

.appRow:hover {
	background: #242c3b;
	border-color: rgba(96, 165, 250, 0.28);
}

.appMain {
	display: flex;
	flex-direction: column;
	gap: 4px;
}

.appName {
	font-weight: 800;
	font-size: 17px;
	line-height: 1.3;
	color: #ffffff;
	word-break: break-word;
}

.appTitle {
	color: rgba(226, 232, 240, 0.78);
	font-size: 14px;
	line-height: 1.4;
	word-break: break-word;
}

.appPath {
	color: rgba(226, 232, 240, 0.62);
	font-size: 13px;
	line-height: 1.45;
	word-break: break-all;
}

.modalHint {
	padding: 14px 18px;
	border-top: 1px solid rgba(255, 255, 255, 0.08);
	background: rgba(255, 255, 255, 0.02);
}

/* Mobile */
@media (max-width: 760px) {
	.app {
		padding: 14px 14px 108px;
	}

	.title {
		font-size: 24px;
	}

	.card {
		padding: 16px;
		border-radius: 18px;
	}

	.card-title {
		font-size: 20px;
	}

	.statsGrid {
		grid-template-columns: 1fr;
	}

	.row-between {
		flex-direction: column;
		align-items: stretch;
	}

	.profileRow {
		align-items: flex-start;
		flex-direction: column;
	}

	.profileCheckInline {
		max-width: 100%;
		justify-content: flex-start;
	}

	.profileCheckInline .checkValue {
		text-align: left;
		white-space: normal;
	}

	.checkRow {
		grid-template-columns: 1fr;
	}

	.checkValue {
		text-align: left;
	}

	.modalOverlay {
		padding: 12px;
		align-items: flex-end;
	}

	.modal {
		max-height: 88vh;
		border-radius: 20px 20px 0 0;
	}

	.fab {
		right: 16px;
		bottom: 92px;
		width: 64px;
		height: 64px;
	}
}
</style>

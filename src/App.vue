<template>
	<div class="app" ref="appRoot">
		<!-- Header -->
		<div class="topbar">
			<button
				v-if="activeTab === 'settings' && settingsSection"
				class="backBtn"
				@click="openSettingsSection(null)"
				title="Назад"
			>
				<span class="backIco">‹</span>
				<span>Настройки</span>
			</button>
			<div class="title">{{ pageTitle }}</div>
		</div>

		<!-- CONTROL TAB -->
		<div v-if="activeTab === 'control'" class="page">
			<div v-if="showUpdateBanner" class="card updateCard">
				<div class="updateHead">
					<span class="updateTitle">Доступна версия {{ update.available?.version }}</span>
					<span v-if="update.available?.mandatory" class="updateBadge">обязательное</span>
				</div>
				<div class="muted updateNotes" v-if="update.available?.releaseNotes">{{ update.available?.releaseNotes }}</div>

				<template v-if="update.downloading">
					<div class="progressBar"><div class="progressFill" :style="{width: updatePercent + '%'}"></div></div>
					<div class="muted updateProgressText">
						Загрузка {{ formatBytes(update.downloadedBytes) }}<template v-if="update.totalBytes"> из {{ formatBytes(update.totalBytes) }}</template>
					</div>
				</template>
				<div v-else-if="update.installing" class="muted">Установка… приложение перезапустится.</div>

				<div class="actions" v-if="!update.installing">
					<button class="btn" @click="installUpdate" :disabled="update.downloading || updateBusy">
						{{ update.ready ? 'Установить и перезапустить' : 'Обновить' }}
					</button>
					<button v-if="!update.available?.mandatory" class="btn btn-ghost" @click="dismissUpdate">Позже</button>
				</div>
				<div class="muted updateHint" v-if="appOptions.autoUpdate && !update.installing">
					Обновление скачается и установится автоматически, когда VPN будет выключен.
				</div>
				<div v-if="update.error" class="error">{{ update.error }}</div>
			</div>

			<div v-if="vpnReconnecting" class="card reconnectCard">
				<span class="pingDot busy"></span>
				<span>{{ vpnMessage || 'Переподключение…' }}</span>
			</div>

			<div v-if="isRunning" class="card">
				<div class="card-title">Статистика</div>

				<div class="statsGrid">
					<div class="statItem">
						<div class="statLabel"><span class="seriesDot down"></span>Скачивание · Down</div>
						<div class="statValue">{{ formatSpeed(dashboard.downBps) }}</div>
					</div>

					<div class="statItem">
						<div class="statLabel"><span class="seriesDot up"></span>Отдача · Up</div>
						<div class="statValue">{{ formatSpeed(dashboard.upBps) }}</div>
					</div>

				</div>
			</div>

			<div v-if="isRunning" class="card">
				<div class="card-title">График трафика</div>
				<TrafficChart :items="trafficHistory"/>
			</div>

			<div v-if="isRunning" class="card">
				<div class="cardHead">
					<svg class="cardHeadIco" viewBox="0 0 24 24" aria-hidden="true">
						<path d="M4 7h11M4 7l3-3M4 7l3 3M20 17H9M20 17l-3-3M20 17l-3 3"/>
					</svg>
					<span class="cardHeadTitle">Активные соединения</span>
				</div>
				<button class="profileSelect" @click="openConnections">
					<span class="profileSelectText">
						{{ dashboard.activeConnections }} {{ pluralConnections(dashboard.activeConnections) }}
					</span>
					<span class="menuChevron">›</span>
				</button>
			</div>

			<div class="card">
				<div class="cardHead">
					<svg class="cardHeadIco" viewBox="0 0 24 24" aria-hidden="true">
						<path d="M7 3h7l5 5v12a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z"/>
						<path d="M14 3v5h5M9.5 13h6M9.5 17h6"/>
					</svg>
					<span class="cardHeadTitle">Профили</span>
				</div>

				<button class="profileSelect" @click="openProfileSheet" :disabled="loadingProfiles">
					<span class="profileSelectText" :class="{ placeholder: !selectedProfile }">
						{{ loadingProfiles ? 'Загрузка…' : (selectedProfile || (profiles.length ? 'Выберите профиль' : 'Профили не загружены')) }}
					</span>
					<svg class="profileSelectChevron" viewBox="0 0 24 24" aria-hidden="true">
						<path d="M8 9.5l4-4 4 4M8 14.5l4 4 4-4"/>
					</svg>
				</button>

				<div v-if="appOptions.autoBestServer && profiles.length" class="muted" style="margin-top:10px">
					Автовыбор: при подключении выбирается лучший сервер по последней проверке,
					а если он перестанет отвечать — следующий.
				</div>
				<div v-if="!profiles.length && !loadingProfiles" class="muted" style="margin-top:12px">
					Добавьте ключ доступа в настройках и нажмите «Обновить конфиги».
				</div>
				<div v-if="errorText && !profileSheetOpen" class="error">{{ errorText }}</div>
			</div>
		</div>

		<!-- SETTINGS TAB -->
		<div v-else class="page">
			<!-- Settings root: access key + list of sections -->
			<template v-if="!settingsSection">
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
						<button class="btn" @click="refreshConfigs" :disabled="loadingConfigs || checkingProfiles">
							{{ loadingConfigs ? 'Обновление…' : 'Обновить конфиги' }}
						</button>
					</div>

					<div v-if="errorText" class="error">{{ errorText }}</div>
				</div>

				<div class="card menuCard">
					<button
						v-for="s in settingsSections"
						:key="s.id"
						class="menuRow"
						@click="openSettingsSection(s.id)"
					>
						<span class="menuIco">{{ s.icon }}</span>
						<span class="menuText">
							<span class="menuTitle">{{ s.title }}</span>
							<span class="menuSub">{{ settingsSummary(s.id) }}</span>
						</span>
						<span class="menuChevron">›</span>
					</button>
				</div>
				<div class="muted versionFooter">ULtunnel {{ update.currentVersion || '—' }}</div>
			</template>

			<!-- Application -->
			<template v-else-if="settingsSection === 'app'">
				<div class="card">
					<div class="card-title">Запуск</div>

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

					<label class="row">
						<input type="checkbox" v-model="appOptions.connectOnLaunch" @change="saveAppOptions()"/>
						<span>Подключаться к VPN при запуске приложения</span>
					</label>
				</div>

				<div class="card">
					<div class="card-title">Подключение</div>

					<label class="row">
						<input type="checkbox" v-model="appOptions.autoReconnect" @change="saveAppOptions()"/>
						<span>Автоматически переподключаться при обрыве</span>
					</label>
					<div class="muted optionHint">
						Если ядро VPN упало или сервер перестал отвечать, приложение переподключится само.
					</div>

					<label class="row">
						<input type="checkbox" v-model="appOptions.autoBestServer" @change="saveAppOptions()"/>
						<span>Автовыбор лучшего сервера</span>
					</label>
					<div class="muted optionHint">
						Подключаться к серверу с лучшей оценкой по последней проверке (пинг и скорость)
						и переключаться на следующий, если текущий не отвечает. Ручной выбор сервера выключает автовыбор.
					</div>

					<label class="row">
						<input type="checkbox" v-model="appOptions.notificationsEnabled" @change="saveAppOptions()"/>
						<span>Системные уведомления</span>
					</label>
					<div class="muted optionHint">
						Обрыв и восстановление соединения, смена сервера, подключение из трея.
					</div>
				</div>

				<div class="card">
					<div class="card-title">Подписка</div>
					<label class="row">
						<input type="checkbox" v-model="appOptions.subscriptionAutoUpdate" @change="saveAppOptions()"/>
						<span>Обновлять список серверов в фоне</span>
					</label>
					<div class="setting-row" v-if="appOptions.subscriptionAutoUpdate">
						<span class="setting-label">Интервал</span>
						<select class="select" v-model.number="appOptions.subscriptionUpdateHours" @change="saveAppOptions()">
							<option v-for="h in subscriptionIntervals" :key="h" :value="h">{{ formatHours(h) }}</option>
						</select>
					</div>
					<div class="muted optionHint">
						Если сервер подписки недоступен, используется сохранённый список.
					</div>
				</div>

				<div class="card">
					<div class="card-title">О приложении</div>
					<div class="kv">
						<span class="k">Версия приложения</span>
						<span class="v">{{ update.currentVersion || '—' }}</span>
					</div>
					<div class="kv">
						<span class="k">Версия ядра</span>
						<span class="v">{{ dashboard.coreVersion }}</span>
					</div>

					<template v-if="update.supported">
						<label class="row">
							<input type="checkbox" v-model="appOptions.autoUpdate" @change="saveAppOptions()"/>
							<span>Устанавливать обновления автоматически</span>
						</label>
						<div class="muted optionHint">
							Новая версия скачивается в фоне и ставится, когда VPN выключен и окно не используется.
							Обязательные обновления ставятся сразу, VPN после перезапуска подключится снова.
						</div>

						<div class="actions">
							<button class="btn btn-ghost" @click="checkUpdates" :disabled="update.checking || update.downloading || update.installing">
								{{ update.checking ? 'Проверка…' : 'Проверить обновления' }}
							</button>
							<button
								v-if="update.available"
								class="btn"
								@click="installUpdate"
								:disabled="update.downloading || update.installing || updateBusy"
							>Обновить до {{ update.available.version }}</button>
						</div>
						<div class="muted" style="margin-top:8px">{{ updateStatusText }}</div>
						<div v-if="update.error" class="error">{{ update.error }}</div>
					</template>
				</div>
			</template>

			<!-- Routing -->
			<template v-else-if="settingsSection === 'routing'">
				<div class="card">
					<label class="row">
						<input type="checkbox" v-model="appOptions.bypassLan" @change="saveAppOptions(true)"/>
						<span>Локальная сеть напрямую</span>
					</label>
					<div class="muted optionHint">
						Адреса 192.168.x.x, 10.x.x.x, 172.16–31.x.x и другие частные диапазоны идут мимо VPN,
						чтобы работали принтеры, NAS и роутер.
					</div>
				</div>

				<div class="card">
					<label class="row">
						<input type="checkbox" v-model="split.enabled" @change="saveSplit"/>
						<span>Раздельная маршрутизация</span>
					</label>
				</div>

				<div class="card">
					<div class="card-title">Приложения через прокси</div>
					<div class="splitBlock">
						<div class="row">
							<button class="btn btn-ghost" @click="openAppsPicker('proxyApps')">Выбрать из запущенных
							</button>
						</div>
						<div class="chips">
							<span class="chip" v-for="a in split.proxyApps" :key="a">
								{{ a }} <button class="chipX" @click="removeFrom('proxyApps', a)">×</button>
							</span>
						</div>
						<div class="muted" v-if="!split.proxyApps.length">Список пуст</div>
					</div>
				</div>

				<div class="card">
					<div class="card-title">Домены через прокси</div>
					<div class="splitBlock">
						<div class="row">
							<input
								class="input"
								v-model="newProxyDomain"
								placeholder="например: google.com"
								@keyup.enter="addTo('proxyDomains','newProxyDomain')"
							/>
							<button class="btn" @click="addTo('proxyDomains','newProxyDomain')">Добавить</button>
						</div>
						<div class="chips">
							<span class="chip" v-for="d in split.proxyDomains" :key="d">
								{{ d }} <button class="chipX" @click="removeFrom('proxyDomains', d)">×</button>
							</span>
						</div>
						<div class="muted" v-if="!split.proxyDomains.length">Список пуст</div>
					</div>
				</div>

				<div class="card">
					<div class="card-title">Браузерный прокси</div>
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

				<div class="card">
					<div class="card-title">Импорт и экспорт</div>
					<div class="muted" style="margin-bottom:12px">
						Раздельная маршрутизация, локальная сеть, браузерный прокси и DNS-серверы в JSON-файле.
					</div>
					<div class="actions">
						<button class="btn btn-ghost" @click="exportRouting" :disabled="routingIoBusy">Экспорт…</button>
						<button class="btn btn-ghost" @click="importRouting" :disabled="routingIoBusy">Импорт…</button>
					</div>
					<div v-if="routingIoNote" :class="routingIoError ? 'error' : 'muted'" style="margin-top:8px">
						{{ routingIoNote }}
					</div>
				</div>
			</template>

			<!-- DNS -->
			<template v-else-if="settingsSection === 'dns'">
				<div class="card">
					<div class="muted" style="margin-bottom:8px">
						Если список пуст, используется DNS из профиля. Первый сервер — основной.
						Форматы: <b>1.1.1.1</b>, <b>tls://dns.google</b>, <b>https://cloudflare-dns.com/dns-query</b>,
						<b>tcp://8.8.8.8</b>.
					</div>
					<div class="row">
						<input
							class="input"
							v-model="newDnsServer"
							placeholder="например: 1.1.1.1 или tls://dns.google"
							@keyup.enter="addDnsServer"
						/>
						<button class="btn" @click="addDnsServer">Добавить</button>
					</div>
					<div class="chips">
						<span class="chip" v-for="d in dnsServers" :key="d">
							{{ d }} <button class="chipX" @click="removeDnsServer(d)">×</button>
						</span>
					</div>
					<div v-if="dnsError" class="error">{{ dnsError }}</div>
				</div>
			</template>

			<!-- Logs -->
			<template v-else-if="settingsSection === 'logs'">
				<div class="card">
					<div class="muted" style="margin-bottom:14px">
						Файл логов приложения откроется в системном просмотрщике.
					</div>
					<button class="btn btn-ghost" @click="openLogs">Открыть логи</button>
					<div v-if="errorText" class="error">{{ errorText }}</div>
				</div>
			</template>
		</div>

		<!-- Floating Start/Stop button (like play button) -->
		<button
			class="fab"
			:class="{ on: vpnActive }"
			:disabled="vpnBusy || checkingProfiles"
			@click="toggleRun"
			:title="vpnActive ? 'Остановить' : 'Запустить'"
		>
			<span class="fabIcon" v-if="!vpnActive">▶</span>
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
				@click="openSettingsTab"
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

			<div class="appsMatchMode">
				<label class="setting-label" for="apps-match-mode">Сопоставлять приложение по</label>
				<select id="apps-match-mode" class="select" v-model="appsMatchMode">
					<option value="path">Абсолютному пути</option>
					<option value="name">Имени исполняемого файла</option>
				</select>
			</div>

			<div class="row appsSearchRow">
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
					:disabled="appsMatchMode === 'path' && !a.path"
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
				<template v-if="appsMatchMode === 'path'">
					Будет создано правило <b>process_path</b> с абсолютным путём к исполняемому файлу.
				</template>
				<template v-else>
					Будет создано правило <b>process_name</b> с именем исполняемого файла.
				</template>
			</div>
		</div>
	</div>


	<!-- Active connections modal -->
	<div v-if="connectionsOpen" class="modalOverlay" @click.self="closeConnections">
		<div class="modal">
			<div class="modalHead">
				<div class="modalTitle">Активные соединения · {{ connections.length }}</div>
				<button class="btn btn-ghost" @click="closeConnections">✕</button>
			</div>

			<div class="row appsSearchRow">
				<input class="input" v-model="connectionsSearch" placeholder="Поиск: домен / IP / процесс / маршрут…"/>
				<button class="btn btn-ghost" @click="closeAllConnections" :disabled="!connections.length">Закрыть все</button>
			</div>

			<div v-if="connectionsError" class="error">{{ connectionsError }}</div>

			<div class="appsList">
				<div v-for="c in filteredConnections" :key="c.id" class="connRow">
					<div class="connMain">
						<div class="connHost">{{ c.host }}<span class="metaDim">:{{ c.port }}</span></div>
						<div class="connMeta">
							<span class="connBadge" :class="c.outbound === split.proxyOutbound || c.outbound === 'proxy' ? 'viaProxy' : 'viaDirect'">
								{{ c.outbound || '—' }}
							</span>
							<span>{{ c.network }}</span>
							<span v-if="c.process">{{ c.process }}</span>
							<span>↓{{ formatBytes(c.download) }} ↑{{ formatBytes(c.upload) }}</span>
						</div>
					</div>
					<div class="connActions">
						<button
							v-if="canAddToProxy(c)"
							class="linkBtn"
							@click="addConnectionDomain(c)"
							title="Добавить домен в «Домены через прокси»"
						>+ в прокси</button>
						<button class="chipX" @click="closeConnection(c.id)" title="Закрыть соединение">×</button>
					</div>
				</div>

				<div class="muted" v-if="!filteredConnections.length">
					{{ connectionsLoaded ? 'Нет соединений' : 'Загрузка…' }}
				</div>
			</div>

			<div class="modalHint muted">
				Метка показывает, куда идёт соединение: <b>proxy</b> — через VPN, <b>direct</b> — напрямую.
			</div>
		</div>
	</div>

	<!-- Profiles bottom sheet -->
	<Transition name="sheet">
		<div v-if="profileSheetOpen" class="sheetOverlay" @click.self="closeProfileSheet">
			<div class="sheet" role="dialog" aria-label="Профили">
				<div class="sheetGrabber" @click="closeProfileSheet"></div>

				<div class="sheetHead">
					<div class="sheetTitle">Профили</div>
					<div class="sheetActions">
						<template v-if="checkingProfiles">
							<span class="sheetProgress">Проверка {{ profileCheckProgress }} / {{ profileCheckTotal || profiles.length }}</span>
							<button class="linkBtn" @click="cancelMeasurement">Остановить</button>
						</template>
						<template v-else>
							<button class="linkBtn" @click="checkProfiles(false)" :disabled="checkDisabled">
								<svg class="linkBtnIco" viewBox="0 0 24 24" aria-hidden="true">
									<path d="M2.5 9a14 14 0 0 1 19 0M5.5 12.5a9.5 9.5 0 0 1 13 0M8.8 16a5 5 0 0 1 6.4 0"/>
									<circle cx="12" cy="19.3" r="1.2"/>
								</svg>
								Проверить
							</button>
							<button class="linkBtn" @click="checkProfiles(true)" :disabled="checkDisabled">
								<svg class="linkBtnIco" viewBox="0 0 24 24" aria-hidden="true">
									<path d="M4 16a8 8 0 1 1 16 0M12 16l4-5"/>
								</svg>
								Скорость
							</button>
						</template>
					</div>
				</div>

				<div v-if="errorText" class="error sheetError">{{ errorText }}</div>

				<div class="sheetList">
					<button
						v-for="p in profileItems"
						:key="p.name"
						class="profileItem"
						:class="['state-' + p.state, { selected: p.name === selectedProfile }]"
						:disabled="checkingProfiles"
						@click="chooseProfile(p.name)"
					>
						<span class="profileItemBody">
							<span class="profileItemName">{{ p.name }}</span>

							<span v-if="p.state === 'ok'" class="profileMeta">
								<span v-if="p.flag" class="metaFlag">{{ p.flag }}</span>
								<span v-if="p.pingMs !== null" class="metaPing">
									<span class="pingDot" :class="pingClass(p.pingMs)"></span>{{ p.pingMs }} мс
								</span>
								<span v-if="p.down !== null">↓{{ p.down }}</span>
								<span v-if="p.up !== null">↑{{ p.up }}</span>
								<span v-if="p.note" class="metaDim">{{ p.note }}</span>
							</span>

							<span v-else-if="p.state === 'fail'" class="profileMeta">
								<span class="metaPing"><span class="pingDot bad"></span>{{ p.note }}</span>
							</span>

							<span v-else-if="p.state === 'checking'" class="profileMeta">
								<span class="metaPing"><span class="pingDot busy"></span>{{ p.note }}</span>
							</span>

							<span v-else-if="p.state === 'pending'" class="profileMeta">
								<span class="metaDim">Ожидает очереди</span>
							</span>
						</span>

						<svg v-if="p.name === selectedProfile" class="profileCheckMark" viewBox="0 0 24 24" aria-hidden="true">
							<path d="M5 12.5l4.5 4.5L19 7.5"/>
						</svg>
					</button>

					<div v-if="!profiles.length" class="muted sheetEmpty">
						Профили не загружены. Нажмите «Обновить конфиги» в настройках.
					</div>
				</div>
			</div>
		</div>
	</Transition>

</template>

<script lang="ts">
import { rankProfiles, serverScore, loadMeasurements, mergeMeasurement as mergeStoredMeasurement } from './serverRating.js'
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
	activeConnections?: number
	coreVersion?: string | null
}

type AppOptions = {
	autoReconnect: boolean
	autoBestServer: boolean
	connectOnLaunch: boolean
	notificationsEnabled: boolean
	subscriptionAutoUpdate: boolean
	subscriptionUpdateHours: number
	bypassLan: boolean
	autoUpdate: boolean
}

type VpnStatus = {
	running: boolean
	reconnecting: boolean
	profile?: string | null
	message?: string | null
}

type ConnectionView = {
	id: string
	host: string
	port: string
	destinationIp: string
	network: string
	process: string
	outbound: string
	rule: string
	upload: number
	download: number
	start: string
}

type ReleaseInfo = {
	version: string
	fileName?: string | null
	sizeBytes?: number | null
	sha256?: string | null
	releaseNotes?: string | null
	mandatory: boolean
	publishedAt?: string | null
}

type UpdateState = {
	currentVersion: string
	supported: boolean
	checking: boolean
	lastCheckedAt?: number | null
	available?: ReleaseInfo | null
	downloading: boolean
	downloadedBytes: number
	totalBytes?: number | null
	ready: boolean
	installing: boolean
	error?: string | null
}

function defaultUpdateState(): UpdateState {
	return {
		currentVersion: '',
		supported: false,
		checking: false,
		lastCheckedAt: null,
		available: null,
		downloading: false,
		downloadedBytes: 0,
		totalBytes: null,
		ready: false,
		installing: false,
		error: null,
	}
}

function readDismissedUpdate(): string {
	try { return localStorage.getItem('updateDismissedVersion') || '' } catch { return '' }
}

type ConnectionsSnapshot = {
	uploadTotal: number
	downloadTotal: number
	connections: ConnectionView[]
}

function defaultAppOptions(): AppOptions {
	return {
		autoReconnect: true,
		autoBestServer: false,
		connectOnLaunch: false,
		notificationsEnabled: true,
		subscriptionAutoUpdate: true,
		subscriptionUpdateHours: 6,
		bypassLan: true,
		autoUpdate: true,
	}
}

function isIpAddress(host: string): boolean {
	return /^\d{1,3}(\.\d{1,3}){3}$/.test(host) || host.includes(':')
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

type AppMatchMode = 'path' | 'name'

type SettingsSectionId = 'app' | 'routing' | 'dns' | 'logs'

const SETTINGS_SECTIONS: { id: SettingsSectionId; title: string; icon: string }[] = [
	{id: 'app', title: 'Приложение', icon: '⚙'},
	{id: 'routing', title: 'Маршрутизация', icon: '⇄'},
	{id: 'dns', title: 'DNS серверы', icon: '◎'},
	{id: 'logs', title: 'Логи', icon: '☰'},
]

type ProfileCheckStatus = 'pending' | 'checking' | 'success' | 'fail'

type ProfileCheckResult = {
	name: string
	ok: boolean
	ip?: string | null
	error?: string | null
    pingMs?: number | null
    jitterMs?: number | null
    connectMs?: number | null
    downloadMbps?: number | null
    uploadMbps?: number | null
    speedError?: string | null
    speedMeasuredAt?: number | null
    measuredAt?: number | null
    country?: string | null
    city?: string | null
	status?: ProfileCheckStatus
}

type ProfileItemView = {
	name: string
	state: 'none' | 'pending' | 'checking' | 'ok' | 'fail'
	flag: string
	pingMs: number | null
	down: number | null
	up: number | null
	note: string
}

function countryFlag(code?: string | null): string {
	if (!code || !/^[A-Za-z]{2}$/.test(code)) return ''
	return String.fromCodePoint(...code.toUpperCase().split('').map(c => 0x1f1e6 + c.charCodeAt(0) - 65))
}

function formatMbps(v?: number | null): number | null {
	if (v == null || !Number.isFinite(v)) return null
	return v >= 10 ? Math.round(v) : Math.round(v * 10) / 10
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
					style: {stroke: 'var(--chart-down)'},
					'stroke-width': 3,
					'stroke-linejoin': 'round',
					'stroke-linecap': 'round',
				}),
				h('path', {
					d: upPath,
					fill: 'none',
					style: {stroke: 'var(--chart-up)'},
					'stroke-width': 3,
					'stroke-linejoin': 'round',
					'stroke-linecap': 'round',
				}),
			]),
			h('div', {class: 'chartLegend'}, [
				h('div', {class: 'legendItem'}, [
					h('span', {class: 'legendSwatch down'}),
					h('span', null, '↓ Down'),
				]),
				h('div', {class: 'legendItem'}, [
					h('span', {class: 'legendSwatch up'}),
					h('span', null, '↑ Up'),
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
		settingsSection: null as SettingsSectionId | null,
		settingsSections: SETTINGS_SECTIONS,
		profileSheetOpen: false,
		// «сейчас» для подписей «N мин.»; обновляется таймером статистики

		isRunning: false,
		// идёт автопереподключение (сообщает бэкенд событием vpn-status)
		vpnReconnecting: false,
		vpnMessage: '' as string,
		appOptions: defaultAppOptions(),
		subscriptionIntervals: [1, 3, 6, 12, 24],
		lastRankingKey: null as string | null,
		appUnlisteners: [] as UnlistenFn[],

		connectionsOpen: false,
		connectionsLoaded: false,
		connections: [] as ConnectionView[],
		connectionsSearch: '' as string,
		connectionsError: '' as string,
		connectionsTimer: null as number | null,

		update: defaultUpdateState(),
		updateBusy: false,
		updateDismissedVersion: readDismissedUpdate(),

		routingIoBusy: false,
		routingIoNote: '' as string,
		routingIoError: false,
		// true, пока выполняется singbox_start_platform/singbox_stop_platform —
		// блокирует повторный клик, который иначе запускал бы второй процесс sing-box.
		vpnBusy: false,

		dashboard: {
			upBps: 0,
			downBps: 0,
			activeConnections: 0,
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
        measuringFull: false,
        measurementCancelling: false,
        measurementsBeforeRun: [] as ProfileCheckResult[],
		profileCheckCurrent: '' as string,
		profileCheckProgress: 0,
		profileCheckTotal: 0,
		profileCheckResults: loadMeasurements() as ProfileCheckResult[],
        speedProgress: {} as Record<string, string>,
        measurementUnlisteners: [] as UnlistenFn[],
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
		appsMatchMode: "path" as AppMatchMode,

		socks5Inbound: false,

		dnsServers: [] as string[],
		newDnsServer: "" as string,
		dnsError: "" as string,

		autostartEnabled: false,
		autostartOsEnabled: false,
		autostartLoading: false,
		autostartNote: '' as string,
	}),

	async created() {
		await this.registerProfileCheckEvents()
		await this.registerVpnEvents()
		await this.bootstrap()
		await this.loadAppOptions()
		await this.loadUpdateState()
		// не ждём: UI уже работает со списком из кеша, свежий подменит его по готовности
		void this.autoRefreshConfigs()
		await this.loadSplit()
		await this.loadSocks5Inbound()
		await this.loadDnsServers()
		await this.loadAutostart()
		this.startStatsPolling()
	},

	mounted() {
		// не в created(): там цепочка await, и упавшая загрузка не дала бы повесить обработчик
		window.addEventListener('keydown', this.onGlobalKeydown)
	},

	beforeUnmount() {
		this.stopStatsPolling()
		this.stopConnectionsPolling()
		this.appUnlisteners.forEach(unlisten => unlisten())
		window.removeEventListener('keydown', this.onGlobalKeydown)
        this.measurementUnlisteners.forEach(unlisten => unlisten())
		if (this.profileCheckUnlisten) {
			this.profileCheckUnlisten()
			this.profileCheckUnlisten = null
		}
	},

	methods: {
		openProfileSheet() {
			this.profileSheetOpen = true
		},

		closeProfileSheet() {
			this.profileSheetOpen = false
		},

		async chooseProfile(name: string) {
			if (this.checkingProfiles) return
			// ручной выбор сервера выключает автовыбор
			if (this.appOptions.autoBestServer) {
				this.appOptions.autoBestServer = false
				await this.saveAppOptions()
			}
			await this.onSelectProfile(name)
			if (!this.errorText) this.closeProfileSheet()
		},

		onGlobalKeydown(e: KeyboardEvent) {
			if (e.key !== 'Escape') return
			if (this.connectionsOpen) this.closeConnections()
			else if (this.appsModalOpen) this.closeAppsPicker()
			else if (this.profileSheetOpen) this.closeProfileSheet()
		},

		pingClass(ms: number): string {
			if (ms < 120) return 'good'
			if (ms < 300) return 'mid'
			return 'bad'
		},

		openSettingsTab() {
			// Повторный клик по «Настройкам» (и переход с другой вкладки) возвращает к списку разделов.
			this.activeTab = 'settings'
			this.openSettingsSection(null)
		},

		openSettingsSection(id: SettingsSectionId | null) {
			this.settingsSection = id
			this.errorText = ''
			const root = this.$refs.appRoot as HTMLElement | undefined
			if (root) root.scrollTop = 0
		},

		settingsSummary(id: SettingsSectionId): string {
			switch (id) {
				case 'app': {
					const parts = [`Версия ${this.update.currentVersion || '—'}`, `автозапуск: ${this.autostartEnabled ? 'вкл' : 'выкл'}`]
					if (this.update.available) parts.push(`доступна ${this.update.available.version}`)
					if (this.appOptions.autoReconnect) parts.push('автопереподключение')
					if (this.appOptions.autoBestServer) parts.push('автовыбор сервера')
					return parts.join(' · ')
				}
				case 'routing': {
					const lan = this.appOptions.bypassLan ? ' · LAN напрямую' : ''
					if (!this.split.enabled) return 'Раздельная маршрутизация выключена' + lan
					const apps = this.split.proxyApps.length
					const domains = this.split.proxyDomains.length
					return `Включена · приложений: ${apps}, доменов: ${domains}` + lan
				}
				case 'dns':
					return this.dnsServers.length
						? `Серверов: ${this.dnsServers.length} · основной ${this.dnsServers[0]}`
						: 'Из профиля'
				case 'logs':
					return 'Файл логов приложения'
			}
		},

        mergeMeasurement(result: ProfileCheckResult, preserveSpeed: boolean): ProfileCheckResult {
            if (this.measurementCancelling && result.error?.includes('прерван')) {
                return this.measurementsBeforeRun.find(r => r.name === result.name) || {name: result.name, ok: false, status: 'pending'}
            }
            return mergeStoredMeasurement(result, this.getProfileCheckResult(result.name), preserveSpeed)
        },
        async cancelMeasurement() { this.measurementCancelling = true; await invoke('cancel_profile_measurement') },
        persistMeasurements() {
            try { localStorage.setItem('serverMeasurements', JSON.stringify(this.profileCheckResults.filter(r => r.status === 'success' || r.status === 'fail'))) } catch { /* Storage may be unavailable. */ }
        },
		async registerProfileCheckEvents() {
            this.measurementUnlisteners.push(await listen<ProfileCheckResult>('server-measurement', event => {
                this.upsertProfileCheckResult(this.mergeMeasurement(event.payload, !this.measuringFull || !event.payload.speedMeasuredAt))
                if (this.measuringFull && event.payload.ok && !event.payload.speedMeasuredAt) {
                    this.speedProgress[event.payload.name] = 'Ожидание замера скорости…'
                } else { delete this.speedProgress[event.payload.name] }
                this.persistMeasurements()
            }))
            this.measurementUnlisteners.push(await listen<{name: string, phase: string, liveMbps: number}>('server-speed-progress', event => {
                const p = event.payload
                this.speedProgress[p.name] = `${p.phase === 'download' ? '↓' : '↑'} ${p.liveMbps.toFixed(1)} Мбит/с…`
            }))
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
				if (payload.status !== 'checking' && this.getProfileCheckResult(payload.name).measuredAt) return
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
			return this.checkingProfiles || this.profileCheckResults.some(x => x.name === name && x.status !== 'pending')
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
			if (this.vpnBusy || this.checkingProfiles) return
			this.vpnBusy = true
			try {
				this.errorText = ''
				if (this.vpnActive) {
					await invoke('vpn_disconnect')
					this.isRunning = false
					this.vpnReconnecting = false
					this.trafficHistory = []
					await this.loadDashboardStats()
					return
				}
				if (!this.selectedProfile && !(this.appOptions.autoBestServer && this.profiles.length)) {
					this.errorText = 'Выберите профиль'
					return
				}
				const profile = await invoke<string>('vpn_connect')
				if (profile) this.selectedProfile = profile
				this.isRunning = true
				this.trafficHistory = []
				await this.loadDashboardStats()
			} catch (e: any) {
				this.errorText = String(e)
				this.isRunning = await invoke<boolean>('get_state').catch(() => false)
			} finally {
				this.vpnBusy = false
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

		async applyLoadedProfiles(list: string[]) {
			this.profiles = Array.isArray(list) ? list : []

			// если раньше выбранный профиль отсутствует — сбросим
			if (this.selectedProfile && !this.profiles.includes(this.selectedProfile)) {
				this.selectedProfile = ''
				await invoke('set_selected_profile', {profile: ''}).catch(() => {
				})
			}
		},

		// Фоновое обновление конфигов при запуске: список из кеша уже показан,
		// при ошибке (нет сети, сервер недоступен) молча остаёмся на нём.
		async autoRefreshConfigs() {
			if (!(this.accessKey || '').trim() || this.loadingConfigs) return
			this.loadingConfigs = true
			try {
				const list = await invoke<string[]>('load_configs')
				await this.applyLoadedProfiles(list)
			} catch (e: any) {
				console.warn('Автообновление конфигов не удалось:', e)
			} finally {
				this.loadingConfigs = false
			}
		},

		async refreshConfigs() {
			try {
				this.errorText = ''
				this.loadingConfigs = true

				// Сохраним ключ перед загрузкой
				await this.saveAccessKey()

				const list = await invoke<string[]>('load_configs')
				await this.applyLoadedProfiles(list)

				// вернём на вкладку управления
				this.activeTab = 'control'
			} catch (e: any) {
				this.errorText = String(e)
			} finally {
				this.loadingConfigs = false
			}
		},

		async checkProfiles(full = false) {
            if (this.checkingProfiles || this.vpnBusy) return
			try {
				this.errorText = ''
				this.checkingProfiles = true
                this.measuringFull = full
                this.measurementCancelling = false
                this.measurementsBeforeRun = [...this.profileCheckResults]
				this.profileCheckCurrent = ''
				this.profileCheckProgress = 0
				this.profileCheckTotal = this.profiles.length
				this.speedProgress = {}
				this.profileCheckResults = this.profiles.map(name => ({
                    ...this.getProfileCheckResult(name),
					name,
					ok: false,
					ip: null,
					error: null,
					status: 'pending' as ProfileCheckStatus,
				}))

				const wasRunning = this.isRunning
				this.isRunning = false
				this.trafficHistory = []

				const results = await invoke<ProfileCheckResult[]>('check_profiles', {full})
				if (Array.isArray(results)) {
                    this.profileCheckResults = results.map(r => this.mergeMeasurement(r, !full || !r.speedMeasuredAt)).filter(r => r.status !== 'pending')
				}
                this.persistMeasurements()
				this.profileCheckCurrent = ''
				this.profileCheckProgress = this.profileCheckTotal || this.profileCheckResults.length
				this.isRunning = await invoke<boolean>('get_state').catch(() => wasRunning)
			} catch (e: any) {
				this.errorText = String(e)
				this.isRunning = await invoke<boolean>('get_state').catch(() => false)
			} finally {
				this.checkingProfiles = false
                this.speedProgress = {}
				this.pushProfileRanking()
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
			await this.restartVpnToApply()
		},

		// Перезапуск VPN, чтобы применить изменённые настройки. Ошибку запуска
		// (например, конфиг не прошёл sing-box check) показываем, а не теряем.
		async restartVpnToApply(): Promise<void> {
			if (!this.isRunning || this.vpnBusy) return
			this.vpnBusy = true
			try {
				await invoke("vpn_restart")
				this.isRunning = true
			} catch (e: any) {
				this.errorText = String(e)
				this.isRunning = await invoke<boolean>('get_state').catch(() => false)
			} finally {
				this.vpnBusy = false
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

		async loadUpdateState() {
			try {
				this.update = {...defaultUpdateState(), ...(await invoke<UpdateState>('get_update_state'))}
			} catch (e: any) {
				console.warn('Состояние обновлений недоступно:', e)
			}
		},

		async checkUpdates() {
			try {
				this.update = {...defaultUpdateState(), ...(await invoke<UpdateState>('check_for_updates'))}
			} catch (e: any) {
				// ошибка уже пришла событием update-state
				console.warn('Проверка обновлений не удалась:', e)
			}
		},

		async installUpdate() {
			if (this.updateBusy) return
			this.updateBusy = true
			try {
				// при успехе приложение закроется и перезапустится установщиком
				await invoke('install_update')
			} catch (e: any) {
				this.update.error = String(e)
			} finally {
				this.updateBusy = false
			}
		},

		dismissUpdate() {
			const v = this.update.available?.version || ''
			this.updateDismissedVersion = v
			try { localStorage.setItem('updateDismissedVersion', v) } catch { /* storage may be unavailable */ }
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

			const value = this.appsMatchMode === "path"
				? (app.path || "").trim()
				: (app.name || "").trim()

			if (!value) return

			if (!this.split[this.appsModalTarget].includes(value)) {
				this.split[this.appsModalTarget].push(value)
				void this.saveSplit()
			}
		},

		async loadDnsServers(): Promise<void> {
			try {
				this.dnsServers = await invoke<string[]>("get_dns_servers")
			} catch (e: any) {
				this.dnsError = String(e)
			}
		},

		async saveDnsServers(next: string[]): Promise<boolean> {
			this.dnsError = ""
			try {
				await invoke("set_dns_servers", {servers: next})
			} catch (e: any) {
				this.dnsError = String(e)
				return false
			}
			this.dnsServers = next

			// применить сразу
			await this.restartVpnToApply()
			return true
		},

		async addDnsServer(): Promise<void> {
			const v = (this.newDnsServer || "").trim()
			if (!v || this.dnsServers.includes(v)) {
				this.newDnsServer = ""
				return
			}
			if (await this.saveDnsServers([...this.dnsServers, v])) {
				this.newDnsServer = ""
			}
		},

		async removeDnsServer(item: string): Promise<void> {
			await this.saveDnsServers(this.dnsServers.filter((x) => x !== item))
		},

		async loadSocks5Inbound(): Promise<void> {
			this.socks5Inbound = await invoke<boolean>("get_socks5_inbound")
		},

		async saveSocks5Inbound(): Promise<void> {
			await invoke("set_socks5_inbound", {enabled: this.socks5Inbound})

			// применить сразу
			await this.restartVpnToApply()
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

		async registerVpnEvents() {
			this.appUnlisteners.push(await listen<VpnStatus>('vpn-status', event => {
				const st = event.payload
				if (!st) return
				const wasRunning = this.isRunning
				this.isRunning = !!st.running
				this.vpnReconnecting = !!st.reconnecting
				this.vpnMessage = st.reconnecting ? (st.message || '') : ''
				if (st.profile) this.selectedProfile = st.profile
				if (st.running && !wasRunning) this.trafficHistory = []
				if (!st.running && !st.reconnecting && st.message && !this.vpnBusy) {
					this.errorText = st.message
				}
			}))
			this.appUnlisteners.push(await listen<string>('profile-selected', event => {
				if (event.payload) this.selectedProfile = event.payload
			}))
			this.appUnlisteners.push(await listen<string[]>('profiles-updated', event => {
				void this.applyLoadedProfiles(event.payload)
			}))
			this.appUnlisteners.push(await listen<UpdateState>('update-state', event => {
				if (event.payload) this.update = {...defaultUpdateState(), ...event.payload}
			}))
			this.appUnlisteners.push(await listen<AppOptions>('app-options-changed', event => {
				if (event.payload) this.appOptions = {...defaultAppOptions(), ...event.payload}
			}))
		},

		async loadAppOptions() {
			try {
				const opts = await invoke<AppOptions>('get_app_options')
				this.appOptions = {...defaultAppOptions(), ...opts}
				const st = await invoke<VpnStatus>('get_vpn_status')
				this.vpnReconnecting = !!st?.reconnecting
			} catch (e: any) {
				console.warn('Настройки приложения недоступны:', e)
			}
		},

		// applyVpn: перезапустить VPN, если изменённая настройка влияет на конфиг ядра
		async saveAppOptions(applyVpn = false) {
			try {
				await invoke('set_app_options', {options: this.appOptions})
				if (applyVpn) await this.restartVpnToApply()
			} catch (e: any) {
				this.errorText = String(e)
			}
		},

		formatHours(h: number): string {
			if (h === 1) return 'каждый час'
			if (h === 24) return 'раз в сутки'
			return `каждые ${h} ч`
		},

		// Бэкенду нужен рейтинг серверов для автовыбора: только серверы с оценкой, лучшие первыми.
		pushProfileRanking() {
			if (this.checkingProfiles) return
			const scored = this.rankedProfiles.filter(name => serverScore(this.getProfileCheckResult(name)) !== null)
			const key = scored.join('\n')
			if (key === this.lastRankingKey) return
			this.lastRankingKey = key
			invoke('set_profile_ranking', {names: scored}).catch(e => console.warn('Рейтинг серверов не сохранён:', e))
		},

		pluralConnections(n: number): string {
			const m10 = n % 10, m100 = n % 100
			if (m10 === 1 && m100 !== 11) return 'соединение'
			if (m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14)) return 'соединения'
			return 'соединений'
		},

		formatBytes(bytes: number): string {
			if (bytes >= 1024 * 1024 * 1024) return (bytes / 1024 / 1024 / 1024).toFixed(2) + ' GB'
			if (bytes >= 1024 * 1024) return (bytes / 1024 / 1024).toFixed(1) + ' MB'
			if (bytes >= 1024) return (bytes / 1024).toFixed(1) + ' KB'
			return bytes + ' B'
		},

		async openConnections() {
			this.connectionsOpen = true
			this.connectionsLoaded = false
			this.connectionsSearch = ''
			await this.loadConnections()
			this.stopConnectionsPolling()
			this.connectionsTimer = window.setInterval(() => void this.loadConnections(), 2000)
		},

		closeConnections() {
			this.connectionsOpen = false
			this.stopConnectionsPolling()
		},

		stopConnectionsPolling() {
			if (this.connectionsTimer !== null) {
				clearInterval(this.connectionsTimer)
				this.connectionsTimer = null
			}
		},

		async loadConnections() {
			if (!this.isRunning) {
				this.connections = []
				this.connectionsLoaded = true
				return
			}
			try {
				const snap = await invoke<ConnectionsSnapshot>('get_connections')
				this.connections = Array.isArray(snap?.connections) ? snap.connections : []
				this.connectionsError = ''
			} catch (e: any) {
				this.connectionsError = String(e)
			} finally {
				this.connectionsLoaded = true
			}
		},

		async closeConnection(id: string) {
			try {
				await invoke('close_connection', {id})
				this.connections = this.connections.filter(c => c.id !== id)
			} catch (e: any) {
				this.connectionsError = String(e)
			}
		},

		async closeAllConnections() {
			try {
				await invoke('close_connection', {id: null})
				await this.loadConnections()
			} catch (e: any) {
				this.connectionsError = String(e)
			}
		},

		canAddToProxy(c: ConnectionView): boolean {
			if (!this.split.enabled || !c.host || isIpAddress(c.host)) return false
			if (c.outbound === this.split.proxyOutbound) return false
			const host = c.host.toLowerCase()
			return !this.split.proxyDomains.some(d => host === d.toLowerCase() || host.endsWith('.' + d.toLowerCase()))
		},

		addConnectionDomain(c: ConnectionView) {
			if (!this.canAddToProxy(c)) return
			this.split.proxyDomains.push(c.host.toLowerCase())
			void this.saveSplit()
		},

		async exportRouting() {
			this.routingIoBusy = true
			this.routingIoNote = ''
			this.routingIoError = false
			try {
				if (await invoke<boolean>('export_routing_settings')) this.routingIoNote = 'Настройки сохранены в файл'
			} catch (e: any) {
				this.routingIoNote = String(e)
				this.routingIoError = true
			} finally {
				this.routingIoBusy = false
			}
		},

		async importRouting() {
			this.routingIoBusy = true
			this.routingIoNote = ''
			this.routingIoError = false
			try {
				if (!(await invoke<boolean>('import_routing_settings'))) return
				await this.loadSplit()
				await this.loadSocks5Inbound()
				await this.loadDnsServers()
				await this.loadAppOptions()
				this.routingIoNote = 'Настройки импортированы'
				await this.restartVpnToApply()
			} catch (e: any) {
				this.routingIoNote = String(e)
				this.routingIoError = true
			} finally {
				this.routingIoBusy = false
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

	watch: {
		rankedProfiles() {
			this.pushProfileRanking()
		},
	},

	computed: {
		showUpdateBanner(): boolean {
			const a = this.update.available
			if (!a) return false
			return a.mandatory || this.update.downloading || this.update.installing || a.version !== this.updateDismissedVersion
		},

		updatePercent(): number {
			const total = this.update.totalBytes || 0
			if (!total) return 0
			return Math.min(100, Math.round(this.update.downloadedBytes / total * 100))
		},

		updateStatusText(): string {
			const u = this.update
			if (u.checking) return 'Проверка обновлений…'
			if (u.installing) return 'Установка обновления…'
			if (u.downloading) return `Загрузка обновления: ${this.updatePercent}%`
			if (u.available) {
				return u.ready
					? `Версия ${u.available.version} скачана и готова к установке`
					: `Доступна версия ${u.available.version}`
			}
			if (u.lastCheckedAt) {
				return `Установлена последняя версия · проверено ${new Date(u.lastCheckedAt * 1000).toLocaleString()}`
			}
			return 'Обновления проверяются автоматически раз в 6 часов'
		},

		vpnActive(): boolean {
			return this.isRunning || this.vpnReconnecting
		},

		filteredConnections(): ConnectionView[] {
			const q = (this.connectionsSearch || '').trim().toLowerCase()
			if (!q) return this.connections
			return this.connections.filter(c =>
				[c.host, c.destinationIp, c.process, c.outbound, c.network, c.rule].some(v => (v || '').toLowerCase().includes(q)))
		},

		checkDisabled(): boolean {
			return this.checkingProfiles || this.vpnBusy || this.loadingProfiles || this.loadingConfigs || !this.profiles.length
		},

		profileItems(): ProfileItemView[] {
			return this.rankedProfiles.map(name => {
				const r = this.getProfileCheckResult(name)
				const shown = this.shouldShowProfileCheckState(name)
				const ok = r.status === 'success' || (!r.status && r.ok)
				const state: ProfileItemView['state'] = !shown ? 'none'
					: r.status === 'pending' ? 'pending'
					: r.status === 'checking' ? 'checking'
					: ok ? 'ok' : 'fail'
				const live = this.speedProgress[name] || ''
				const note = state === 'ok' ? (live || r.speedError || '')
					: state === 'checking' ? (live || 'Проверка…')
					: state === 'fail' ? (r.error || 'Ошибка проверки')
					: ''
				return {
					name,
					state,
					flag: countryFlag(r.country),
					pingMs: r.pingMs ?? null,
					down: formatMbps(r.downloadMbps),
					up: formatMbps(r.uploadMbps),
					note,
				}
			})
		},

        rankedProfiles(): string[] { return rankProfiles(this.profiles, this.profileCheckResults) },
		pageTitle(): string {
			if (this.activeTab === 'control') return 'Панель управления'
			const section = this.settingsSections.find(s => s.id === this.settingsSection)
			return section ? section.title : 'Настройки'
		},

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

/* Палитра как в мобильном клиенте: нейтральный тёмный фон, светло-голубой акцент */
.app,
.sheetOverlay,
.modalOverlay {
	--bg: #111216;
	--surface-1: #1b1c21;
	--surface-2: #25272e;
	--surface-3: #2d3038;
	--surface-sheet: #16171b;
	--text: #e4e5ea;
	--text-dim: #a9acb6;
	--text-faint: #7a7e89;
	--accent: #a8c4ff;
	--accent-soft: rgba(168, 196, 255, 0.12);
	--good: #7fd4a0;
	/* цвета серий графика трафика: линии, легенда и плитки статистики */
	--chart-down: #60a5fa;
	--chart-up: #c084fc;
}

.app {
	margin: 0;
	/* без верхнего отступа: иначе липкий хэдер прилипает ниже края окна и в зазор видно контент */
	padding: 0 16px 110px;
	height: 100vh;
	overflow-y: auto;
	overflow-x: hidden;
	background: var(--bg);
	color: var(--text);
	font-family: system-ui, -apple-system, Segoe UI, Roboto, Arial, sans-serif;
}

/* Top */
.topbar {
	position: sticky;
	top: 0;
	z-index: 20;
	/* растягиваем фон хэдера на горизонтальные отступы .app */
	margin: 0 -16px;
	/* по горизонтали выравниваем по колонке .page (max-width 760px) */
	padding: 20px max(16px, calc((100% - 760px) / 2 + 16px)) 14px;
	background: rgba(17, 18, 22, 0.94);
	backdrop-filter: blur(10px);
}

.title {
	font-size: 28px;
	font-weight: 600;
	line-height: 1.1;
	letter-spacing: -0.01em;
	color: var(--text);
}

.backBtn {
	display: inline-flex;
	align-items: center;
	gap: 4px;
	margin: 0 0 6px -6px;
	padding: 4px 8px;
	border: none;
	border-radius: 10px;
	background: transparent;
	color: var(--accent);
	font-size: 15px;
	font-weight: 600;
	cursor: pointer;
	transition: background 0.16s ease;
}

.backBtn:hover {
	background: rgba(79, 140, 255, 0.10);
}

.backIco {
	font-size: 24px;
	line-height: 0.8;
}

/* Settings menu */
.menuCard {
	padding: 6px;
}

.menuRow {
	width: 100%;
	display: flex;
	align-items: center;
	gap: 14px;
	padding: 14px 12px;
	border: none;
	border-radius: 14px;
	background: transparent;
	color: inherit;
	text-align: left;
	cursor: pointer;
	transition: background 0.16s ease;
}

.menuRow + .menuRow {
	box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.06);
}

.menuRow:hover {
	background: rgba(255, 255, 255, 0.05);
}

.menuIco {
	flex: 0 0 auto;
	width: 40px;
	height: 40px;
	border-radius: 12px;
	display: flex;
	align-items: center;
	justify-content: center;
	background: var(--accent-soft);
	color: var(--accent);
	font-size: 19px;
}

.menuText {
	flex: 1 1 auto;
	min-width: 0;
	display: flex;
	flex-direction: column;
	gap: 3px;
}

.menuTitle {
	font-size: 16px;
	font-weight: 700;
	color: #ffffff;
}

.menuSub {
	font-size: 13px;
	color: rgba(226, 232, 240, 0.62);
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
}

.menuChevron {
	flex: 0 0 auto;
	font-size: 24px;
	line-height: 1;
	color: rgba(226, 232, 240, 0.4);
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
	background: var(--surface-1);
	border-radius: 24px;
	padding: 20px;
}

.card-title {
	font-weight: 600;
	font-size: 19px;
	line-height: 1.2;
	margin-bottom: 14px;
	color: var(--text);
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
	background: var(--surface-2);
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
	background: var(--accent);
	color: #10213f;
	border: none;
	border-radius: 999px;
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
	background: var(--surface-2);
	color: var(--text);
	border: none;
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
	background: var(--surface-2);
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


.profileCheckStatus {
	margin-top: 14px;
	padding: 14px;
	border-radius: 16px;
	background: var(--surface-2);
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
	background: var(--surface-2);
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

/* Profile selector (control tab) */
.cardHead {
	display: flex;
	align-items: center;
	gap: 12px;
	margin-bottom: 16px;
}

.cardHeadIco {
	width: 24px;
	height: 24px;
	fill: none;
	stroke: var(--text-dim);
	stroke-width: 1.7;
	stroke-linecap: round;
	stroke-linejoin: round;
}

.cardHeadTitle {
	font-size: 19px;
	font-weight: 600;
	color: var(--text-dim);
}

.profileSelect {
	width: 100%;
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 12px;
	min-height: 58px;
	padding: 14px 18px;
	border: none;
	border-radius: 16px;
	background: var(--surface-2);
	color: var(--text);
	font: inherit;
	text-align: left;
	cursor: pointer;
	transition: background 0.16s ease;
}

.profileSelect:hover:not(:disabled) {
	background: var(--surface-3);
}

.profileSelect:disabled {
	cursor: default;
	opacity: 0.7;
}

.profileSelectText {
	font-size: 17px;
	font-weight: 500;
	letter-spacing: 0.01em;
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
}

.profileSelectText.placeholder {
	color: var(--text-faint);
}

.profileSelectChevron {
	flex: 0 0 auto;
	width: 22px;
	height: 22px;
	fill: none;
	stroke: var(--text-dim);
	stroke-width: 1.8;
	stroke-linecap: round;
	stroke-linejoin: round;
}

/* Profiles bottom sheet */
.sheetOverlay {
	position: fixed;
	inset: 0;
	z-index: 40;
	display: flex;
	align-items: flex-end;
	justify-content: center;
	background: rgba(0, 0, 0, 0.55);
}

.sheet {
	width: min(760px, 100%);
	max-height: 86vh;
	display: flex;
	flex-direction: column;
	background: var(--surface-sheet);
	border-radius: 28px 28px 0 0;
	box-shadow: 0 -12px 40px rgba(0, 0, 0, 0.45);
	padding: 0 16px calc(16px + env(safe-area-inset-bottom));
}

.sheetGrabber {
	width: 36px;
	height: 4px;
	margin: 14px auto 18px;
	border-radius: 999px;
	background: rgba(226, 228, 236, 0.45);
	cursor: pointer;
}

.sheetHead {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 12px;
	padding: 4px 10px 16px;
}

.sheetTitle {
	font-size: 26px;
	font-weight: 700;
	color: var(--text);
}

.sheetActions {
	display: flex;
	align-items: center;
	gap: 6px;
	flex-wrap: wrap;
	justify-content: flex-end;
}

.sheetProgress {
	font-size: 14px;
	color: var(--text-dim);
	margin-right: 4px;
}

.linkBtn {
	display: inline-flex;
	align-items: center;
	gap: 8px;
	padding: 8px 12px;
	border: none;
	border-radius: 999px;
	background: transparent;
	color: var(--accent);
	font: inherit;
	font-size: 15px;
	font-weight: 600;
	cursor: pointer;
	transition: background 0.16s ease;
}

.linkBtn:hover:not(:disabled) {
	background: var(--accent-soft);
}

.linkBtn:disabled {
	opacity: 0.45;
	cursor: default;
}

.linkBtnIco {
	width: 20px;
	height: 20px;
	fill: none;
	stroke: currentColor;
	stroke-width: 2;
	stroke-linecap: round;
	stroke-linejoin: round;
}

.linkBtnIco circle {
	fill: currentColor;
	stroke: none;
}

.sheetError {
	margin: 0 0 12px;
}

.sheetList {
	display: flex;
	flex-direction: column;
	gap: 6px;
	overflow-y: auto;
	min-height: 0;
	padding-bottom: 4px;
}

.sheetEmpty {
	padding: 12px 10px;
}

.profileItem {
	display: flex;
	align-items: center;
	gap: 12px;
	width: 100%;
	padding: 14px 18px;
	border: none;
	border-radius: 16px;
	background: var(--surface-1);
	color: var(--text);
	font: inherit;
	text-align: left;
	cursor: pointer;
	transition: background 0.16s ease;
}

.profileItem:hover:not(:disabled) {
	background: var(--surface-2);
}

.profileItem.selected {
	background: var(--surface-2);
}

.profileItem:disabled {
	cursor: default;
}

.profileItemBody {
	flex: 1 1 auto;
	min-width: 0;
	display: flex;
	flex-direction: column;
	gap: 6px;
}

.profileItemName {
	font-size: 17px;
	font-weight: 500;
	color: var(--text);
	word-break: break-word;
}

.profileMeta {
	display: flex;
	align-items: center;
	flex-wrap: wrap;
	column-gap: 14px;
	row-gap: 4px;
	font-size: 14px;
	font-weight: 500;
	color: var(--text-dim);
	font-variant-numeric: tabular-nums;
}


.metaPlace,
.metaPing {
	display: inline-flex;
	align-items: center;
	gap: 6px;
	min-width: 0;
}

.metaFlag {
	font-size: 15px;
	line-height: 1;
}

.metaDim {
	color: var(--text-faint);
}

.state-fail .metaPing {
	color: #f2a7a7;
	word-break: break-word;
}

.pingDot {
	width: 8px;
	height: 8px;
	flex: 0 0 auto;
	border-radius: 999px;
	background: var(--good);
}

.pingDot.mid {
	background: #e8c36a;
}

.pingDot.bad {
	background: #ef7a7a;
}

.pingDot.busy {
	background: var(--accent);
	animation: pulse 1s ease-in-out infinite;
}

@keyframes pulse {
	50% { opacity: 0.3; }
}

.profileCheckMark {
	flex: 0 0 auto;
	width: 26px;
	height: 26px;
	fill: none;
	stroke: var(--accent);
	stroke-width: 2.2;
	stroke-linecap: round;
	stroke-linejoin: round;
}

.sheet-enter-active,
.sheet-leave-active {
	transition: background 0.22s ease;
}

.sheet-enter-active .sheet,
.sheet-leave-active .sheet {
	transition: transform 0.26s cubic-bezier(0.2, 0.8, 0.2, 1);
}

.sheet-enter-from,
.sheet-leave-to {
	background: rgba(0, 0, 0, 0);
}

.sheet-enter-from .sheet,
.sheet-leave-to .sheet {
	transform: translateY(100%);
}

/* Stats */
.statsGrid {
	display: grid;
	grid-template-columns: repeat(2, minmax(0, 1fr));
	gap: 14px;
}

.statItem {
	background: var(--surface-2);
	border: 1px solid rgba(255, 255, 255, 0.10);
	border-radius: 18px;
	padding: 16px;
	min-height: 106px;
	display: flex;
	flex-direction: column;
	justify-content: center;
}

.statLabel {
	display: flex;
	align-items: center;
	gap: 8px;
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

/* TrafficChart рендерит внутренние элементы сам, scoped-атрибут есть только у корня .chartWrap,
   поэтому стили внутренних элементов — через :deep() */
.chartWrap :deep(.trafficSvg) {
	width: 100%;
	height: 240px;
	display: block;
	background: var(--surface-2);
	border: 1px solid rgba(255, 255, 255, 0.10);
	border-radius: 18px;
}

.chartWrap :deep(.chartLegend) {
	display: flex;
	gap: 20px;
	flex-wrap: wrap;
}

.chartWrap :deep(.legendItem) {
	display: inline-flex;
	align-items: center;
	gap: 8px;
	font-size: 14px;
	font-weight: 500;
	color: var(--text-dim);
}

.chartWrap :deep(.legendSwatch) {
	width: 18px;
	height: 4px;
	border-radius: 999px;
	display: inline-block;
}

.chartWrap :deep(.legendSwatch.down),
.seriesDot.down {
	background: var(--chart-down);
}

.chartWrap :deep(.legendSwatch.up),
.seriesDot.up {
	background: var(--chart-up);
}

.seriesDot {
	width: 10px;
	height: 10px;
	border-radius: 999px;
	display: inline-block;
	flex: 0 0 auto;
}

.chartEmpty {
	height: 240px;
	display: flex;
	align-items: center;
	justify-content: center;
	background: var(--surface-2);
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
	background: rgba(22, 23, 27, 0.96);
	backdrop-filter: blur(12px);
	border-top: none;
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
	background: var(--accent-soft);
	color: var(--accent);
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
	background: var(--surface-sheet);
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

.appsMatchMode {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 16px;
	padding: 16px 20px 0;
}

.appsMatchMode .select {
	min-width: 250px;
}

.appsSearchRow {
	padding: 16px 20px 0;
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

.appRow:disabled {
	opacity: 0.45;
	cursor: not-allowed;
}

.appRow:disabled:hover {
	background: #1e2431;
	border-color: rgba(255, 255, 255, 0.10);
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

.updateCard {
	border: 1px solid rgba(168, 196, 255, 0.35);
}

.updateHead {
	display: flex;
	align-items: center;
	gap: 10px;
	margin-bottom: 6px;
}

.updateTitle {
	font-weight: 700;
	font-size: 16px;
}

.updateBadge {
	padding: 1px 8px;
	border-radius: 999px;
	font-size: 12px;
	font-weight: 600;
	background: rgba(248, 113, 113, 0.15);
	color: #fca5a5;
}

.updateNotes {
	white-space: pre-line;
	margin-bottom: 10px;
}

.updateProgressText {
	margin-top: 6px;
}

.updateHint {
	margin-top: 8px;
	font-size: 12.5px;
}

.versionFooter {
	text-align: center;
	margin: 4px 0 12px;
	font-size: 12.5px;
}

.reconnectCard {
	display: flex;
	align-items: center;
	gap: 10px;
	color: var(--text-dim);
}

.optionHint {
	margin: -4px 0 12px 28px;
	font-size: 12.5px;
}

.connRow {
	display: flex;
	align-items: center;
	gap: 10px;
	padding: 10px 12px;
	border-radius: 12px;
	background: var(--surface-2);
}

.connMain {
	flex: 1;
	min-width: 0;
}

.connHost {
	font-weight: 600;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.connMeta {
	display: flex;
	flex-wrap: wrap;
	gap: 4px 10px;
	margin-top: 4px;
	font-size: 12px;
	color: var(--text-dim);
}

.connBadge {
	padding: 0 6px;
	border-radius: 6px;
	font-weight: 600;
}

.connBadge.viaProxy {
	background: var(--accent-soft);
	color: var(--accent);
}

.connBadge.viaDirect {
	background: rgba(127, 212, 160, 0.12);
	color: var(--good);
}

.connActions {
	display: flex;
	align-items: center;
	gap: 6px;
	flex-shrink: 0;
}

.modalHint {
	padding: 14px 18px;
	border-top: 1px solid rgba(255, 255, 255, 0.08);
	background: rgba(255, 255, 255, 0.02);
}

/* Mobile */
@media (max-width: 760px) {
	.app {
		padding: 0 14px 108px;
	}

	.topbar {
		margin: 0 -14px;
		padding: 18px 14px 14px;
	}

	.title {
		font-size: 24px;
	}

	.appsMatchMode {
		align-items: stretch;
		flex-direction: column;
		gap: 8px;
	}

	.appsMatchMode .select {
		width: 100%;
		min-width: 0;
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

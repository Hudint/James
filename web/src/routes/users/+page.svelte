<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
  import { confirmAction } from '$lib/confirm.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import Tabs from '$lib/components/Tabs.svelte';
  import { i18n, locales, t } from '$lib/i18n/index.svelte';
  import { reportError, session, toast } from '$lib/session.svelte';
  import type { Grant, Role, User } from '$lib/types';

  /** Permissions known to the server, in display order. */
  const PERMISSIONS = [
    'playback.play',
    'playback.control',
    'playback.volume',
    'queue.manage',
    'server.move',
    'server.message',
    'server.chat.read',
    'instance.view',
    'instance.manage',
    'library.read',
    'library.manage',
    'playlist.read',
    'playlist.manage',
    'tts.say',
    'plugins.manage',
    'plugins.use',
    'users.manage',
  ] as const;
  let permLabels = $derived(locales[i18n.locale].users.perms as Record<string, string>);

  let users = $state<User[] | null>(null);
  let roles = $state<Role[]>([]);
  let tab = $state<'users' | 'roles'>('users');

  let showNewUser = $state(false);
  let newName = $state('');
  let newPassword = $state('');
  let newRoles = $state<string[]>([]);

  let editing = $state<number | null>(null);
  let editRoles = $state<string[]>([]);
  let editPassword = $state('');
  let editAdmin = $state(false);
  let editDisabled = $state(false);

  /** Grants being edited per role (global grants as checkboxes, everything else as text). */
  let roleGrants = $state<Record<string, Grant[]>>({});
  let advanced = $state<Record<string, boolean>>({});
  let newRoleName = $state('');

  function grantsToText(grants: Grant[]): string {
    return grants.map((g) => (g.instance ? `${g.permission}@${g.instance}` : g.permission)).join('\n');
  }

  function textToGrants(text: string): Grant[] {
    return text
      .split('\n')
      .map((line) => line.trim())
      .filter(Boolean)
      .map((line) => {
        const [permission, instance] = line.split('@');
        return { permission: permission.trim(), instance: instance?.trim() || null };
      });
  }

  function hasGlobal(role: string, permission: string): boolean {
    return (roleGrants[role] ?? []).some((g) => g.permission === permission && g.instance === null);
  }

  function toggleGlobal(role: string, permission: string, on: boolean) {
    const grants = (roleGrants[role] ?? []).filter((g) => !(g.permission === permission && g.instance === null));
    if (on) grants.push({ permission, instance: null });
    roleGrants[role] = grants;
  }

  async function load() {
    try {
      [users, roles] = await Promise.all([api.users(), api.roles()]);
      roleGrants = Object.fromEntries(roles.map((r) => [r.name, r.grants.map((g) => ({ ...g }))]));
    } catch (e) {
      reportError(e);
      users ??= [];
    }
  }

  onMount(load);

  /** Runs an action; returns whether it succeeded (forms are only reset on success). */
  async function run(action: () => Promise<unknown>, success?: string): Promise<boolean> {
    try {
      await action();
      if (success) toast(success, 'success');
      await load();
      return true;
    } catch (e) {
      reportError(e);
      return false;
    }
  }

  async function createUser(event: SubmitEvent) {
    event.preventDefault();
    const ok = await run(() => api.createUser({ username: newName, password: newPassword || undefined, roles: newRoles }));
    if (ok) {
      newName = '';
      newPassword = '';
      newRoles = [];
      showNewUser = false;
    }
  }

  function startEdit(user: User) {
    editing = user.id;
    editRoles = [...user.roles];
    editPassword = '';
    editAdmin = user.isAdmin;
    editDisabled = user.disabled;
  }

  async function saveEdit(event: SubmitEvent, user: User) {
    event.preventDefault();
    const body: Parameters<typeof api.updateUser>[1] = { roles: editRoles };
    if (editPassword) body.password = editPassword;
    if (editAdmin !== user.isAdmin) body.isAdmin = editAdmin;
    if (editDisabled !== user.disabled) body.disabled = editDisabled;
    if (await run(() => api.updateUser(user.id, body), t('users.saved'))) editing = null;
  }

  async function deleteUser(user: User) {
    if (await confirmAction(t('users.deleteConfirm', { name: user.username }), t('common.delete'))) {
      await run(() => api.deleteUser(user.id));
    }
  }

  async function createRole(event: SubmitEvent) {
    event.preventDefault();
    if (await run(() => api.createRole(newRoleName, '', []))) newRoleName = '';
  }

  async function deleteRole(role: Role) {
    if (await confirmAction(t('users.deleteRoleConfirm', { name: role.name }), t('common.delete'))) {
      await run(() => api.deleteRole(role.name));
    }
  }

  /** Saves one role and refreshes only that role, keeping unsaved edits of the others. */
  async function saveRole(role: Role) {
    try {
      const updated = await api.setRoleGrants(role.name, roleGrants[role.name] ?? []);
      roleGrants[role.name] = updated.grants.map((g) => ({ ...g }));
      const index = roles.findIndex((r) => r.name === role.name);
      if (index >= 0) roles[index] = updated;
      toast(t('users.saved'), 'success');
    } catch (e) {
      reportError(e);
    }
  }

  function instanceGrants(role: string): number {
    return (roleGrants[role] ?? []).filter((g) => g.instance !== null).length;
  }

  function roleDescription(role: Role): string {
    const builtin = locales[i18n.locale].users.roleDescriptions as Record<string, string>;
    return role.builtin ? (builtin[role.name] ?? role.description) : role.description;
  }

  const assignable = $derived(roles.filter((r) => r.name !== 'everyone'));
</script>

<svelte:head><title>{t('users.title')} · James</title></svelte:head>

<div class="page">
  <header class="page-header">
    <h1>{t('users.title')}</h1>
    {#if tab === 'users' && !showNewUser}
      <button class="btn btn-primary" onclick={() => (showNewUser = true)}><Icon name="plus" size={16} />{t('users.new')}</button>
    {/if}
  </header>

  <Tabs
    bind:value={tab}
    label={t('users.title')}
    tabs={[
      { value: 'users', label: t('users.users'), icon: 'user' },
      { value: 'roles', label: t('users.roles'), icon: 'key' },
    ]}
  />

  {#if users === null}
    <p class="muted" aria-busy="true">{t('common.loading')}</p>
  {:else if tab === 'users'}
    <div id="panel-users" role="tabpanel" aria-labelledby="tab-users" class="stack">
      {#if showNewUser}
        <form class="card stack" onsubmit={createUser}>
          <h2>{t('users.new')}</h2>
          <div class="form-grid">
            <div class="field">
              <label for="new-user">{t('auth.username')}</label>
              <!-- svelte-ignore a11y_autofocus -->
              <input id="new-user" type="text" required maxlength="64" bind:value={newName} autocomplete="off" autofocus />
            </div>
            <div class="field">
              <label for="new-password">{t('auth.password')}</label>
              <input id="new-password" type="password" minlength="8" bind:value={newPassword} autocomplete="new-password" aria-describedby="new-password-hint" />
              <span id="new-password-hint" class="hint">{t('auth.passwordHint')}</span>
            </div>
          </div>
          <fieldset class="row">
            <legend class="label">{t('users.rolesLabel')}</legend>
            {#each assignable as role (role.id)}
              <label class="checkbox"><input type="checkbox" value={role.name} bind:group={newRoles} />{role.name}</label>
            {/each}
          </fieldset>
          <div class="row">
            <button class="btn btn-primary" type="submit" disabled={!newName.trim()}>{t('common.create')}</button>
            <button class="btn" type="button" onclick={() => (showNewUser = false)}>{t('common.cancel')}</button>
          </div>
        </form>
      {/if}

      <ul class="card list users">
        {#each users as user (user.id)}
          <li class="user">
            <div class="head">
              <Icon name="user" />
              <span class="name truncate">{user.username}</span>
              {#if user.isAdmin}<span class="badge accent">{t('users.admin')}</span>{/if}
              {#if user.disabled}<span class="badge danger">{t('users.disabled')}</span>{/if}
              {#each user.roles as role (role)}<span class="badge">{role}</span>{/each}
              <span class="spacer"></span>
              {#if editing !== user.id}
                <button class="btn btn-small" onclick={() => startEdit(user)}>{t('common.edit')}</button>
                {#if user.id !== session.me?.user.id}
                  <button class="icon-btn small" onclick={() => deleteUser(user)} aria-label="{t('common.delete')}: {user.username}"><Icon name="trash" size={16} /></button>
                {/if}
              {/if}
            </div>
            {#if editing === user.id}
              <form class="edit stack" onsubmit={(e) => saveEdit(e, user)}>
                <fieldset class="row">
                  <legend class="label">{t('users.rolesLabel')}</legend>
                  {#each assignable as role (role.id)}
                    <label class="checkbox"><input type="checkbox" value={role.name} bind:group={editRoles} />{role.name}</label>
                  {/each}
                </fieldset>
                {#if user.id !== session.me?.user.id}
                  <div class="form-grid">
                    <div class="field">
                      <label for="edit-password-{user.id}">{t('users.setPassword')} <span class="muted">({t('common.optional')})</span></label>
                      <input id="edit-password-{user.id}" type="password" minlength="8" bind:value={editPassword} autocomplete="new-password" />
                    </div>
                  </div>
                  {#if session.me?.user.isAdmin}
                    <label class="checkbox"><input type="checkbox" bind:checked={editAdmin} />{t('users.admin')}</label>
                  {/if}
                  <label class="checkbox"><input type="checkbox" bind:checked={editDisabled} />{t('users.disabled')}</label>
                {/if}
                <div class="row">
                  <button class="btn btn-primary" type="submit">{t('common.save')}</button>
                  <button class="btn" type="button" onclick={() => (editing = null)}>{t('common.cancel')}</button>
                </div>
              </form>
            {/if}
          </li>
        {/each}
      </ul>
    </div>
  {:else}
    <div id="panel-roles" role="tabpanel" aria-labelledby="tab-roles" class="stack">
      <form class="card row nowrap-row" onsubmit={createRole}>
        <label class="visually-hidden" for="new-role">{t('users.newRole')}</label>
        <input id="new-role" class="grow" type="text" placeholder={t('users.newRole')} pattern="[A-Za-z0-9_\-]+" bind:value={newRoleName} />
        <button class="btn btn-primary" type="submit" disabled={!newRoleName.trim()}><Icon name="plus" size={16} />{t('common.create')}</button>
      </form>
      <div class="roles">
        {#each roles as role (role.id)}
          <section class="card stack" aria-labelledby="role-{role.id}">
            <div class="card-header">
              <h2 id="role-{role.id}">{role.name}</h2>
              {#if role.builtin}<span class="badge">{t('users.builtin')}</span>{/if}
              {#if !role.builtin}
                <button class="icon-btn small" onclick={() => deleteRole(role)} aria-label="{t('common.delete')}: {role.name}"><Icon name="trash" size={16} /></button>
              {/if}
            </div>
            {#if roleDescription(role)}<p class="muted small">{roleDescription(role)}</p>{/if}
            {#if advanced[role.name]}
              <div class="field">
                <label for="grants-{role.id}">{t('users.permissions')}</label>
                <textarea
                  id="grants-{role.id}"
                  spellcheck="false"
                  value={grantsToText(roleGrants[role.name] ?? [])}
                  onchange={(e) => (roleGrants[role.name] = textToGrants(e.currentTarget.value))}
                  aria-describedby="grants-hint-{role.id}"
                ></textarea>
                <span id="grants-hint-{role.id}" class="hint">{t('users.permissionsHint')}</span>
              </div>
            {:else}
              {#if hasGlobal(role.name, '*')}
                <p class="small full-access">{t('users.fullAccess')}</p>
              {:else}
              <fieldset class="perms">
                <legend class="label">{t('users.permissions')}</legend>
                {#each PERMISSIONS as permission (permission)}
                  <label class="checkbox perm">
                    <input
                      type="checkbox"
                      checked={hasGlobal(role.name, permission)}
                      onchange={(e) => toggleGlobal(role.name, permission, e.currentTarget.checked)}
                    />
                    <span>{permLabels[permission] ?? permission}<span class="code muted">{permission}</span></span>
                  </label>
                {/each}
                {#if instanceGrants(role.name) > 0}
                  <p class="hint">{t('users.instanceGrants', { count: instanceGrants(role.name) })}</p>
                {/if}
              </fieldset>
              {/if}
            {/if}
            <div class="row">
              <button class="btn btn-small btn-primary" onclick={() => saveRole(role)}>{t('common.save')}</button>
              <label class="checkbox small"><input type="checkbox" bind:checked={advanced[role.name]} />{t('users.advanced')}</label>
            </div>
          </section>
        {/each}
      </div>
    </div>
  {/if}
</div>

<style>
  .users li.user {
    flex-direction: column;
    align-items: stretch;
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
    min-height: 40px;
  }
  .name {
    font-weight: 600;
  }
  .edit {
    padding: var(--space-3) 0 var(--space-2) var(--space-6);
  }
  fieldset.row {
    gap: var(--space-4);
  }
  fieldset legend {
    margin-bottom: var(--space-2);
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .nowrap-row {
    flex-wrap: nowrap;
  }
  .roles {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
    gap: var(--space-4);
  }
  .perms {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .perm {
    align-items: flex-start;
  }
  .code {
    display: block;
    font-family: var(--mono);
    font-size: var(--text-xs);
  }
  .full-access {
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius);
    background: var(--accent-soft);
    color: var(--accent);
  }
  @media (max-width: 600px) {
    .edit {
      padding-left: 0;
    }
  }
</style>

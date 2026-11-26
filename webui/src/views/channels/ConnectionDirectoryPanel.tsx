import React, { useCallback, useEffect, useRef, useState } from "react";
import { Alert, Button, Card, Input, Select, Space, Spin, Tag } from "antd";
import { api } from "../../api";
import {
  ConnectionDirectoryError,
  type ConnectionDirectoryPage,
  type ConnectionDirectoryQuery,
  type ConnectionDirectoryRecord,
  type ConnectionSourceKind,
} from "../../api/nexus";
import { BrandEmpty } from "../../components/BrandEmpty";
import { useT } from "../../i18n/core";
import "./ConnectionDirectoryPanel.css";

const PAGE_SIZE = 50;

interface DirectoryState {
  page: ConnectionDirectoryPage | null;
  loading: boolean;
  error: string | null;
  stale: boolean;
  cursorExpired: boolean;
  queryKey: string | null;
  pageIndex: number;
}

function queryKey(query: ConnectionDirectoryQuery): string {
  return JSON.stringify({ search: query.search ?? "", kind: query.kind ?? "", sourceKind: query.sourceKind ?? "" });
}

function identityKey(record: ConnectionDirectoryRecord): string {
  const source = record.sourceRef;
  const scope = source.scope;
  const lifecycle = source.lifecycle;
  const reference = record.reference;
  return JSON.stringify([
    record.kind, source.kind, source.sourceId, scope.instanceId, scope.authorityId,
    scope.accountId, scope.projectId ?? null, source.subresourceId ?? null,
    lifecycle.state, lifecycle.state === "known" ? lifecycle.generation : null,
    reference?.instanceId ?? null, reference?.connectionId ?? null, reference?.generation ?? null,
  ]);
}

function errorMessage(error: unknown, t: ReturnType<typeof useT>): string {
  if (error instanceof ConnectionDirectoryError) {
    if (error.code === "unsupported") return t("nexus.catalogUnsupported");
    if (error.code === "forbidden") return t("nexus.catalogForbidden");
    if (error.code === "cursor_expired") return t("nexus.catalogCursorExpired");
    if (error.code === "invalid_response") return t("nexus.catalogInvalid");
  }
  return t("nexus.catalogFailed", { error: error instanceof Error ? error.message : String(error) });
}

function displayValue(value: string | null | undefined): string {
  return value?.trim() || "—";
}

function relationEndpointLabel(endpoint: unknown): string {
  if (!endpoint || typeof endpoint !== "object") return "—";
  if ("displayName" in endpoint && typeof endpoint.displayName === "string") return endpoint.displayName;
  if ("connectionId" in endpoint && typeof endpoint.connectionId === "string") return endpoint.connectionId;
  return "—";
}

function DirectoryRecordCard({
  record,
  onOpenSource,
}: {
  record: ConnectionDirectoryRecord;
  onOpenSource: (surface: string, sourceId: string) => void;
}): React.ReactElement {
  const t = useT();
  const source = record.sourceRef;
  const editTarget = record.sourceEditTarget;
  const titleId = `connection-${encodeURIComponent(identityKey(record))}`;
  const relations = record.relations ?? [];
  return (
    <Card size="small" className="connection-directory-card hud">
      <details className="connection-directory-details">
        <summary aria-labelledby={titleId}>
          <span className="connection-directory-summary-main">
            <strong id={titleId} className="connection-directory-name">{record.displayName}</strong>
            <span className="connection-directory-tags">
              <Tag>{t(`nexus.catalogKind.${record.kind}`)}</Tag>
              <Tag>{t(`nexus.catalogSourceKind.${source.kind}`)}</Tag>
              <Tag>{t(`nexus.catalogAvailability.${record.readiness.availability}`)}</Tag>
              <Tag>{t(`nexus.catalogFreshness.${record.readiness.freshness}`)}</Tag>
            </span>
          </span>
          <span className="connection-directory-id mono">{source.sourceId}</span>
        </summary>
        <div className="connection-directory-detail">
          <div className="connection-directory-section">
            <h3>{t("nexus.catalogIdentity")}</h3>
            <dl>
              <dt>{t("nexus.catalogKindLabel")}</dt><dd>{t(`nexus.catalogKind.${record.kind}`)} · {t(`nexus.catalogSourceKind.${source.kind}`)}</dd>
              <dt>{t("nexus.catalogSourceId")}</dt><dd className="mono">{source.sourceId}</dd>
              <dt>{t("nexus.catalogInstance")}</dt><dd className="mono">{source.scope.instanceId}</dd>
              <dt>{t("nexus.catalogAuthority")}</dt><dd className="mono">{source.scope.authorityId}</dd>
              <dt>{t("nexus.catalogAccount")}</dt><dd className="mono">{source.scope.accountId}</dd>
              <dt>{t("nexus.catalogProject")}</dt><dd className="mono">{displayValue(source.scope.projectId)}</dd>
              <dt>{t("nexus.catalogSubresource")}</dt><dd className="mono">{displayValue(source.subresourceId)}</dd>
              <dt>{t("nexus.catalogLifecycle")}</dt>
              <dd className="mono">{source.lifecycle.state === "known" ? `${source.lifecycle.state} · ${source.lifecycle.generation}` : source.lifecycle.state}</dd>
              <dt>{t("nexus.catalogRevision")}</dt><dd className="mono">{source.revision}</dd>
              {record.reference && <><dt>{t("nexus.catalogConnectionRef")}</dt><dd className="mono">{JSON.stringify(record.reference)}</dd></>}
              {record.registrationState && <><dt>{t("nexus.catalogRegistration")}</dt><dd>{record.registrationState}</dd></>}
            </dl>
          </div>

          <div className="connection-directory-section">
            <h3>{t("nexus.catalogReadiness")}</h3>
            <div className="connection-directory-tags">
              <Tag>{record.readiness.enabled ? t("nexus.catalogEnabled") : t("nexus.catalogDisabled")}</Tag>
              <Tag>{t(`nexus.catalogHandshake.${record.readiness.handshake}`)}</Tag>
              {record.readiness.reasons.map((reason) => <Tag key={reason}>{t(`nexus.catalogReason.${reason}`)}</Tag>)}
            </div>
            {record.device && (
              <div className="connection-directory-tags">
                <Tag>{t("nexus.catalogDeviceKind", { value: record.device.kind })}</Tag>
                <Tag>{t("nexus.catalogDeviceStatus", { value: record.device.status })}</Tag>
                <Tag>{t("nexus.catalogDevicePairing", { value: record.device.pairing })}</Tag>
                <Tag>{t("nexus.catalogDeviceBackend", { value: record.device.backend })}</Tag>
              </div>
            )}
          </div>

          <div className="connection-directory-section">
            <h3>{t("nexus.catalogCapabilities", { count: record.capabilitiesTotal ?? record.capabilities.length })}</h3>
            {record.capabilities.length === 0 ? <p className="dim">{t("nexus.catalogNoCapabilities")}</p> : (
              <ul className="connection-directory-capabilities">
                {record.capabilities.map((capability, index) => (
                  <li key={`${capability.name}:${capability.role}:${index}`}>
                    <strong>{capability.name}</strong>
                    <span>{t("nexus.catalogCapabilityMeta", {
                      role: capability.role,
                      origin: capability.origin,
                      effects: capability.effects.join(", ") || "—",
                      verification: capability.verification,
                      authorization: capability.authorization,
                    })}</span>
                  </li>
                ))}
              </ul>
            )}
            {record.capabilitiesTruncated && <p className="dim">{t("nexus.catalogCapabilitiesTruncated", { count: record.capabilitiesTotal })}</p>}
          </div>

          <div className="connection-directory-section">
            <h3>{t("nexus.catalogRelations", { count: record.relationsTotal ?? relations.length })}</h3>
            {relations.length === 0 ? <p className="dim">{t("nexus.catalogNoRelations")}</p> : (
              <ul className="connection-directory-relations">
                {relations.map((entry, index) => {
                  const relation = entry.relation ?? entry;
                  const from = entry.from;
                  const to = entry.to;
                  return (
                    <li key={relation.id ?? `${relation.kind ?? "relation"}:${index}`}>
                      <Tag>{t(`nexus.catalogRelationKind.${relation.kind ?? "unknown"}`)}</Tag>
                      <span>{from?.displayName ?? relationEndpointLabel(relation.from)}</span>
                      <span aria-hidden="true">→</span>
                      <span>{to?.displayName ?? relationEndpointLabel(relation.to)}</span>
                      {relation.revision !== undefined && <span className="mono dim">r{relation.revision}</span>}
                      {(from?.reference ?? relation.from) && <code>{JSON.stringify(from?.reference ?? relation.from)}</code>}
                      {(to?.reference ?? relation.to) && <code>{JSON.stringify(to?.reference ?? relation.to)}</code>}
                    </li>
                  );
                })}
              </ul>
            )}
            {record.relationsTruncated && <p className="dim">{t("nexus.catalogRelationsTruncated")}</p>}
            <p className="connection-directory-boundary-note">{t("nexus.catalogRelationBoundary")}</p>
          </div>

          <div className="connection-directory-source-action">
            <div>
              <strong>{t("nexus.catalogSourceManagement")}</strong>
              <div className="mono dim">{editTarget.surface} · {editTarget.sourceId}</div>
            </div>
            {editTarget.surface === "nexus_links" ? (
              <Button onClick={() => onOpenSource(editTarget.surface, editTarget.sourceId)}>{t("nexus.catalogOpenSource")}</Button>
            ) : (
              <Button disabled title={t("nexus.catalogSourceNotReady")}>{t("nexus.catalogSourceNotReady")}</Button>
            )}
          </div>
        </div>
      </details>
    </Card>
  );
}

export function ConnectionDirectoryPanel({
  onOpenSource,
}: {
  onOpenSource: (surface: string, sourceId: string) => void;
}): React.ReactElement {
  const t = useT();
  const [searchDraft, setSearchDraft] = useState("");
  const [kind, setKind] = useState<ConnectionDirectoryQuery["kind"]>();
  const [sourceKind, setSourceKind] = useState<ConnectionSourceKind>();
  const currentQuery = useRef<ConnectionDirectoryQuery>({ limit: PAGE_SIZE });
  /** Request history keeps the offset and cursor pair for each rendered page. */
  const pageRequests = useRef<ConnectionDirectoryQuery[]>([{ limit: PAGE_SIZE, offset: 0 }]);
  const generation = useRef(0);
  const controller = useRef<AbortController | null>(null);
  const lastRequest = useRef<{ query: ConnectionDirectoryQuery; pageIndex: number }>({
    query: currentQuery.current,
    pageIndex: 0,
  });
  const translate = useRef(t);
  translate.current = t;
  const [state, setState] = useState<DirectoryState>({
    page: null,
    loading: false,
    error: null,
    stale: false,
    cursorExpired: false,
    queryKey: null,
    pageIndex: 0,
  });

  const loadPage = useCallback(async (
    query: ConnectionDirectoryQuery,
    pageIndex: number,
  ) => {
    lastRequest.current = { query, pageIndex };
    controller.current?.abort();
    const requestController = new AbortController();
    controller.current = requestController;
    const requestGeneration = ++generation.current;
    const normalized = { ...query, limit: PAGE_SIZE };
    const requestedKey = queryKey(query);
    setState((current) => ({
      ...current,
      loading: true,
      error: null,
      cursorExpired: false,
      stale: current.page !== null && (
        current.error !== null || current.stale
        || current.queryKey !== requestedKey || current.pageIndex !== pageIndex
      ),
    }));
    try {
      const page = await api.connectionDirectory(normalized, requestController.signal);
      if (requestController.signal.aborted || generation.current !== requestGeneration) return;
      setState({ page, loading: false, error: null, stale: false, cursorExpired: false, queryKey: requestedKey, pageIndex });
    } catch (error) {
      if (requestController.signal.aborted || generation.current !== requestGeneration) return;
      setState((current) => ({
        ...current,
        loading: false,
        error: errorMessage(error, translate.current),
        stale: current.page !== null,
        cursorExpired: error instanceof ConnectionDirectoryError && error.code === "cursor_expired",
      }));
    }
  }, []);

  useEffect(() => {
    // The first request may omit offset, while its saved page position is explicit for
    // deterministic back-navigation and compatibility with older query handling.
    pageRequests.current = [{ ...currentQuery.current, offset: 0 }];
    void loadPage(pageRequests.current[0], 0);
    return () => {
      generation.current += 1;
      controller.current?.abort();
      controller.current = null;
    };
  }, [loadPage]);

  const applyQuery = (query: ConnectionDirectoryQuery) => {
    const normalized = { ...query, limit: PAGE_SIZE };
    currentQuery.current = normalized;
    pageRequests.current = [{ ...normalized, offset: 0 }];
    void loadPage(normalized, 0);
  };

  const refreshDirectory = () => {
    if (state.cursorExpired) {
      // A 409 means the cursor can no longer identify the retained snapshot. Only an
      // explicit user action starts a fresh first-page query; keep all active filters.
      const firstPage: ConnectionDirectoryQuery = {
        search: currentQuery.current.search,
        kind: currentQuery.current.kind,
        sourceKind: currentQuery.current.sourceKind,
        limit: PAGE_SIZE,
        offset: 0,
      };
      currentQuery.current = firstPage;
      pageRequests.current = [firstPage];
      void loadPage(firstPage, 0);
      return;
    }
    void loadPage(lastRequest.current.query, lastRequest.current.pageIndex);
  };

  const page = state.page;
  return (
    <section className="connection-directory" aria-label={t("nexus.catalogTitle")}>
      <Alert
        type="info"
        showIcon
        title={t("nexus.catalogBoundary")}
        className="connection-directory-boundary"
      />
      <div className="connection-directory-filters">
        <Input.Search
          allowClear
          aria-label={t("nexus.catalogSearch")}
          placeholder={t("nexus.catalogSearch")}
          value={searchDraft}
          onChange={(event) => setSearchDraft(event.target.value)}
          onSearch={(value) => applyQuery({ ...currentQuery.current, search: value || undefined })}
          className="connection-directory-search"
        />
        <Select
          allowClear
          aria-label={t("nexus.catalogType")}
          placeholder={t("nexus.catalogType")}
          value={kind}
          options={[
            { value: "software", label: t("nexus.catalogKind.software") },
            { value: "external_agent", label: t("nexus.catalogKind.external_agent") },
            { value: "device", label: t("nexus.catalogKind.device") },
          ]}
          onChange={(value) => {
            setKind(value);
            applyQuery({ ...currentQuery.current, kind: value || undefined });
          }}
          className="connection-directory-filter"
        />
        <Select
          allowClear
          aria-label={t("nexus.catalogSource")}
          placeholder={t("nexus.catalogSource")}
          value={sourceKind}
          options={(["nexus", "mcp", "plugin", "device", "software"] as ConnectionSourceKind[])
            .map((value) => ({ value, label: t(`nexus.catalogSourceKind.${value}`) }))}
          onChange={(value) => {
            setSourceKind(value);
            applyQuery({ ...currentQuery.current, sourceKind: value || undefined });
          }}
          className="connection-directory-filter connection-directory-source-filter"
        />
        <div className="connection-directory-actions">
          <Button loading={state.loading} onClick={refreshDirectory}>
            {state.cursorExpired ? t("nexus.catalogRecheck") : t("common.refresh")}
          </Button>
          <Button
            onClick={() => {
              setSearchDraft("");
              setKind(undefined);
              setSourceKind(undefined);
              // Keep the reset request explicit at the first server page. The initial
              // request may omit offset, but every subsequent page has an offset/cursor pair.
              applyQuery({ limit: PAGE_SIZE, offset: 0 });
            }}
          >
            {t("nexus.catalogReset")}
          </Button>
        </div>
      </div>

      {state.error && (
        <Alert
          type={state.stale ? "warning" : "error"}
          showIcon
          title={state.cursorExpired ? t("nexus.catalogCursorExpired") : state.stale ? t("nexus.catalogStale") : state.error}
          description={state.stale && !state.cursorExpired ? state.error : undefined}
          action={<Button size="small" onClick={refreshDirectory}>{state.cursorExpired ? t("nexus.catalogRecheck") : t("common.refresh")}</Button>}
          className="connection-directory-error"
        />
      )}
      {state.stale && !state.error && <Alert type="warning" showIcon title={t("nexus.catalogStale")} className="connection-directory-error" />}

      <Spin spinning={state.loading}>
        {page ? (
          <>
            {page.issues.length > 0 && (
              <Alert
                type="warning"
                showIcon
                title={t("nexus.catalogIssues", { count: page.issues.length })}
                description={<ul className="connection-directory-issues">{page.issues.map((issue, index) => (
                  <li key={`${issue.source}:${issue.reason}:${index}`}>{t(`nexus.catalogSourceKind.${issue.source}`)} · {t(`nexus.catalogReason.${issue.reason}`)}</li>
                ))}</ul>}
                className="connection-directory-issues-alert"
              />
            )}
            {page.records.length === 0 ? (
              <BrandEmpty description={t("nexus.catalogEmpty")} className="pane-empty" />
            ) : (
              <div className="connection-directory-records" aria-live="polite">
                {page.records.map((record) => (
                  <DirectoryRecordCard key={identityKey(record)} record={record} onOpenSource={onOpenSource} />
                ))}
              </div>
            )}
            <div className="connection-directory-pagination">
              <span>{t("nexus.catalogCount", { count: page.total })}</span>
              <Button
                disabled={state.pageIndex === 0 || state.loading || state.stale}
                onClick={() => {
                  const targetIndex = state.pageIndex - 1;
                  const request = pageRequests.current[targetIndex];
                  if (request) void loadPage(request, targetIndex);
                }}
              >{t("nexus.catalogPrevious")}</Button>
              <Button
                disabled={!page.truncated || !page.nextCursor || state.loading || state.stale}
                onClick={() => {
                  if (!page.nextCursor || page.nextOffset === null) return;
                  const targetIndex = state.pageIndex + 1;
                  const request = {
                    ...currentQuery.current,
                    offset: page.nextOffset,
                    cursor: page.nextCursor,
                  };
                  pageRequests.current = [...pageRequests.current.slice(0, targetIndex), request];
                  void loadPage(request, targetIndex);
                }}
              >{t("nexus.catalogNext")}</Button>
            </div>
          </>
        ) : state.loading ? (
          <div className="connection-directory-loading" role="status">{t("nexus.catalogLoading")}</div>
        ) : state.error ? null : (
          <BrandEmpty description={t("nexus.catalogEmpty")} className="pane-empty" />
        )}
      </Spin>
      <Space className="connection-directory-auth-note" wrap>
        <Tag>{t("nexus.catalogManagementOnly")}</Tag>
        <Tag>{t("nexus.catalogExecutionUnknown")}</Tag>
      </Space>
    </section>
  );
}

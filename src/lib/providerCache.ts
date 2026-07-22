import type { ProviderInfo } from '../types/generated/ProviderInfo';

let providerListPromise: Promise<ProviderInfo[]> | null = null;
const defaultProviderByMesh = new Map<number, Promise<string>>();

export function getProviderListPromise(): Promise<ProviderInfo[]> | null {
  return providerListPromise;
}

export function setProviderListPromise(promise: Promise<ProviderInfo[]> | null): void {
  providerListPromise = promise;
}

export function getDefaultProviderPromise(meshId: number): Promise<string> | undefined {
  return defaultProviderByMesh.get(meshId);
}

export function setDefaultProviderPromise(meshId: number, promise: Promise<string>): void {
  defaultProviderByMesh.set(meshId, promise);
}

export function deleteDefaultProviderPromise(meshId: number): void {
  defaultProviderByMesh.delete(meshId);
}

export function resetProviderCachesForTests(): void {
  providerListPromise = null;
  defaultProviderByMesh.clear();
}

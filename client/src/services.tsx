/** Presentational chat components consume the surrounding client's transport. */
import { createContext, useContext } from "react";
import type { ExmClient } from "./api/client";
import type { Translate } from "./translate";

type Services = Pick<ExmClient, "assetUrl" | "downloadUrl"> & {
  api: Pick<ExmClient["api"], "sessionTokens" | "undoSession">;
  t: Translate;
  dark: boolean;
};
export const ClientServices = createContext<Services | null>(null);
export function useClientServices(): Services {
  const services = useContext(ClientServices);
  if (!services) throw new Error("ClientServices provider is missing");
  return services;
}

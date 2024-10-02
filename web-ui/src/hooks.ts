import React from "react";
import { Config } from "worterbuch-react";

export const NamespaceContext = React.createContext<string | undefined>(
  undefined
);

export type ExtendedConfig = Config & { namespace: string };

export function useNamespace(): string | undefined {
  return React.useContext(NamespaceContext);
}

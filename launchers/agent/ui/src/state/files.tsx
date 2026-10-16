import { createContext, useContext } from "react";

export const FileNavigation = createContext<{
  cwd: string;
  openFile: (path: string, trigger: HTMLElement) => void;
} | null>(null);

export const useFileNavigation = () => useContext(FileNavigation);

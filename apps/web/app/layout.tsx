import type { Metadata } from "next";
import type { ReactNode } from "react";
import { Providers } from "../components/providers";
import "./styles.css";

export const metadata: Metadata = {
  title: "nanoctl",
  description: "Fast, secure remote access to your computers.",
};

export default function RootLayout({ children }: Readonly<{ children: ReactNode }>) {
  return (
    <html lang="en">
      <body>
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}

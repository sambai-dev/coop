import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Rookhold code runner",
  description: "A small bounded-code starter built on Rookhold",
};

export default function Layout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}

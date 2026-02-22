import { Routes, Route, Navigate } from 'react-router-dom'
import { useAuth } from './hooks/useAuth'
import Login from './pages/Login'
import Dashboard from './pages/Dashboard'
import TenantList from './pages/TenantList'
import TenantDetail from './pages/TenantDetail'
import SetupWizard from './pages/SetupWizard'
import UserList from './pages/UserList'
import AuditLog from './pages/AuditLog'
import ToastProvider from './components/ToastProvider'
import Toast from './components/Toast'

function ProtectedRoute({ children }: { children: React.ReactNode }) {
  const { isLoggedIn } = useAuth()
  if (!isLoggedIn) return <Navigate to="/login" replace />
  return <>{children}</>
}

function AdminRoute({ children }: { children: React.ReactNode }) {
  const { isLoggedIn, isSuperAdmin } = useAuth()
  if (!isLoggedIn) return <Navigate to="/login" replace />
  if (!isSuperAdmin) return <Navigate to="/tenants" replace />
  return <>{children}</>
}

export default function App() {
  return (
    <ToastProvider>
      <Routes>
        <Route path="/login" element={<Login />} />
        <Route path="/" element={<AdminRoute><Dashboard /></AdminRoute>} />
        <Route path="/tenants" element={<ProtectedRoute><TenantList /></ProtectedRoute>} />
        <Route path="/tenants/:id" element={<ProtectedRoute><TenantDetail /></ProtectedRoute>} />
        <Route path="/tenants/:id/setup" element={<ProtectedRoute><SetupWizard /></ProtectedRoute>} />
        <Route path="/users" element={<AdminRoute><UserList /></AdminRoute>} />
        <Route path="/audit" element={<AdminRoute><AuditLog /></AdminRoute>} />
      </Routes>
      <Toast />
    </ToastProvider>
  )
}

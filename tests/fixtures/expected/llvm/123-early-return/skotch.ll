; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @InputKt_abs(i32 %arg0) {
entry:
  %t0 = add i32 0, 0
  %t1 = icmp slt i32 %arg0, %t0
  br i1 %t1, label %bb1, label %bb2
bb1:
  %t2 = add i32 0, 0
  %t3 = sub i32 %t2, %arg0
  ret i32 %t3
bb2:
  br label %bb3
bb3:
  ret i32 %arg0
}

define i32 @InputKt_max(i32 %arg0, i32 %arg1) {
entry:
  %t0 = icmp sgt i32 %arg0, %arg1
  br i1 %t0, label %bb1, label %bb2
bb1:
  ret i32 %arg0
bb2:
  br label %bb3
bb3:
  ret i32 %arg1
}

define i32 @main() {
entry:
  %t0 = add i32 0, -42
  %t1 = call i32 @InputKt_abs(i32 %t0)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t1)
  %t3 = add i32 0, 7
  %t4 = call i32 @InputKt_abs(i32 %t3)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t4)
  %t6 = add i32 0, 10
  %t7 = add i32 0, 20
  %t8 = call i32 @InputKt_max(i32 %t6, i32 %t7)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t8)
  %t10 = add i32 0, 99
  %t11 = add i32 0, 1
  %t12 = call i32 @InputKt_max(i32 %t10, i32 %t11)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t12)
  ret i32 0
}


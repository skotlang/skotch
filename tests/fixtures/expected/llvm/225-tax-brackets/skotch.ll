; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @InputKt_tax(i32 %arg0) {
entry:
  %merge_2 = alloca i32
  %t0 = add i32 0, 1
  br label %bb1
bb1:
  %t1 = add i32 0, 10000
  %t2 = icmp sle i32 %arg0, %t1
  br i1 %t2, label %bb2, label %bb3
bb2:
  %t3 = add i32 0, 0
  store i32 %t3, ptr %merge_2
  br label %bb8
bb3:
  %t4 = add i32 0, 40000
  %t5 = icmp sle i32 %arg0, %t4
  br i1 %t5, label %bb4, label %bb5
bb4:
  %t6 = add i32 0, 10000
  %t7 = sub i32 %arg0, %t6
  %t8 = add i32 0, 10
  %t9 = mul i32 %t7, %t8
  %t10 = add i32 0, 100
  %t11 = sdiv i32 %t9, %t10
  store i32 %t11, ptr %merge_2
  br label %bb8
bb5:
  %t12 = add i32 0, 80000
  %t13 = icmp sle i32 %arg0, %t12
  br i1 %t13, label %bb6, label %bb7
bb6:
  %t14 = add i32 0, 3000
  %t15 = add i32 0, 40000
  %t16 = sub i32 %arg0, %t15
  %t17 = add i32 0, 20
  %t18 = mul i32 %t16, %t17
  %t19 = add i32 0, 100
  %t20 = sdiv i32 %t18, %t19
  %t21 = add i32 %t14, %t20
  store i32 %t21, ptr %merge_2
  br label %bb8
bb7:
  %t22 = add i32 0, 11000
  %t23 = add i32 0, 80000
  %t24 = sub i32 %arg0, %t23
  %t25 = add i32 0, 30
  %t26 = mul i32 %t24, %t25
  %t27 = add i32 0, 100
  %t28 = sdiv i32 %t26, %t27
  %t29 = add i32 %t22, %t28
  store i32 %t29, ptr %merge_2
  br label %bb8
bb8:
  %t30 = load i32, ptr %merge_2
  ret i32 %t30
}

define i32 @main() {
entry:
  %t0 = add i32 0, 5000
  %t1 = call i32 @InputKt_tax(i32 %t0)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t1)
  %t3 = add i32 0, 25000
  %t4 = call i32 @InputKt_tax(i32 %t3)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t4)
  %t6 = add i32 0, 60000
  %t7 = call i32 @InputKt_tax(i32 %t6)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t7)
  %t9 = add i32 0, 100000
  %t10 = call i32 @InputKt_tax(i32 %t9)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t10)
  ret i32 0
}

